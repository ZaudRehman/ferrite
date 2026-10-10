//! Page scripts. Each installs once, failures stay silent, reports never block.

pub const TRACKING_PARAMS: &[&str] = &[
    "gclid",
    "fbclid",
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "mc_eid",
    "mc_cid",
    "igshid",
    "ttclid",
    "twclid",
];

/// Same set as the injected script, for server side use (history writes,
/// autocomplete). Returns input unchanged when it does not parse.
pub fn strip_tracking_params(raw: &str) -> String {
    let Ok(mut url) = url::Url::parse(raw) else {
        return raw.to_owned();
    };
    if !raw.contains('?') {
        return raw.to_owned();
    }
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !TRACKING_PARAMS.contains(&k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.len() == url.query_pairs().count() {
        return raw.to_owned();
    }
    url.query_pairs_mut().clear();
    if kept.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut()
            .extend_pairs(kept.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    url.into()
}

pub fn param_stripping_js() -> String {
    let params = TRACKING_PARAMS
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"(function() {{
  try {{
    var params = [{params}];
    var url = new URL(window.location.href);
    var changed = false;
    for (var i = 0; i < params.length; i++) {{
      if (url.searchParams.has(params[i])) {{
        url.searchParams.delete(params[i]);
        changed = true;
      }}
    }}
    if (changed) window.history.replaceState(null, "", url.toString());
  }} catch(e) {{}}
}})();"#,
        params = params
    )
}

pub fn deamp_js() -> String {
    r#"(function() {
  try {
    var ampLink = document.querySelector('link[rel="amphtml"]');
    var isAmp = location.hostname.endsWith(".amp.project.net") || !!document.querySelector('html[amp]') || !!ampLink;
    if (isAmp) {
      var canonical = document.querySelector('link[rel="canonical"]');
      if (canonical && canonical.href) {
        window.location.replace(canonical.href);
        return;
      }
      if (ampLink && ampLink.href) {
        window.location.replace(ampLink.href);
      }
    }
  } catch(e) {}
})();"#
        .to_string()
}

pub fn cosmetic_css(selectors: &[&str]) -> String {
    if selectors.is_empty() {
        return String::new();
    }
    let joined = selectors.join(", ");
    format!("{joined} {{ display: none !important; }}")
}

pub fn cosmetic_js(css: &str) -> String {
    let escaped = css
        .replace('\\', "\\\\")
        .replace('`', "\\`")
        .replace('$', "\\$");
    format!(
        r#"(function() {{
  try {{
    var css = `{escaped}`;
    if (!css) return;
    var style = document.createElement("style");
    style.textContent = css;
    (document.head || document.documentElement).appendChild(style);
    var obs = new MutationObserver(function(muts) {{
      for (var i = 0; i < muts.length; i++) {{
        for (var j = 0; j < muts[i].addedNodes.length; j++) {{
          var n = muts[i].addedNodes[j];
          if (n.nodeType === 1 && n.matches) {{
            try {{ if (n.matches(`{css}`.split(',')[0].trim())) n.style.setProperty("display","none","important"); }} catch(e) {{}}
          }}
        }}
      }}
    }});
    obs.observe(document.documentElement, {{childList:true, subtree:true}});
  }} catch(e) {{}}
}})();"#,
        escaped = escaped,
        css = css
    )
}

/// Page asks the host for selectors. Once per document, http(s) only.
/// Payloads stay capped, so a page cannot solicit a huge style tag.
pub fn cosmetic_query_js() -> String {
    r#"(function() {
  try {
    if (window.__ferriteCosmeticAsked) return;
    window.__ferriteCosmeticAsked = true;
    var p = window.location.protocol;
    if (p !== "http:" && p !== "https:") return;
    if (!window.ipc || !window.ipc.postMessage) return;
    window.ipc.postMessage(JSON.stringify({type:"cosmetic_query", url:window.location.href}));
  } catch(e) {}
})();"#
        .to_string()
}

/// Report password forms posting cross host. Report only, never blocks here.
/// Host revalidates before storing. Once per (page, action) per document.
pub fn form_risk_js() -> String {
    r#"(function() {
  try {
    if (window.__ferriteFormRisk) return;
    window.__ferriteFormRisk = {};
    if (!window.ipc || !window.ipc.postMessage) return;
    document.addEventListener("submit", function(ev) {
      try {
        var form = ev.target;
        if (!form || form.tagName !== "FORM") return;
        if (!form.querySelector('input[type="password"]')) return;
        var action = form.action || window.location.href;
        var pageHost = window.location.hostname.toLowerCase();
        var actionHost = new URL(action, window.location.href).hostname.toLowerCase();
        if (!actionHost || actionHost === pageHost) return;
        var key = pageHost + ">" + actionHost;
        if (window.__ferriteFormRisk[key]) return;
        window.__ferriteFormRisk[key] = true;
        window.ipc.postMessage(JSON.stringify({type:"form_risk", page:window.location.href, action:action}));
      } catch(e) {}
    }, true);
  } catch(e) {}
})();"#
        .to_string()
}

pub fn fingerprint_spoof_js() -> String {
    r#"(function() {
  try {
    // Canvas
    var origToDataURL = HTMLCanvasElement.prototype.toDataURL;
    HTMLCanvasElement.prototype.toDataURL = function() {
      var ctx = this.getContext("2d");
      if (ctx) {
        try {
          var img = ctx.getImageData(0,0,1,1);
          img.data[0] = (img.data[0] + Math.floor(Math.random()*5)) % 256;
          ctx.putImageData(img,0,0);
        } catch(e) {}
      }
      return origToDataURL.apply(this, arguments);
    };
    var origGetImageData = CanvasRenderingContext2D.prototype.getImageData;
    CanvasRenderingContext2D.prototype.getImageData = function() {
      var data = origGetImageData.apply(this, arguments);
      for (var i = 0; i < data.data.length; i+=4) {
        data.data[i] = (data.data[i] + (i % 5)) % 256;
      }
      return data;
    };
    // WebGL
    var origGetParam = WebGLRenderingContext.prototype.getParameter;
    if (origGetParam) {
      WebGLRenderingContext.prototype.getParameter = function(pname) {
        if (pname === 37445) return "Google Inc.";
        if (pname === 37446) return "ANGLE (Intel)";
        return origGetParam.apply(this, arguments);
      };
    }
    // Audio
    if (window.AudioContext || window.webkitAudioContext) {
      var AC = window.AudioContext || window.webkitAudioContext;
      var origCreateAnalyser = AC.prototype.createAnalyser;
      if (origCreateAnalyser) {
        AC.prototype.createAnalyser = function() {
          var analyser = origCreateAnalyser.apply(this, arguments);
          var origGetFloat = analyser.getFloatFrequencyData;
          if (origGetFloat) {
            analyser.getFloatFrequencyData = function(arr) {
              var ret = origGetFloat.apply(this, arguments);
              for (var i = 0; i < arr.length; i++) arr[i] += (Math.random()-0.5)*0.0001;
              return ret;
            };
          }
          return analyser;
        };
      }
    }
  } catch(e) {}
})();"#
        .to_string()
}

/// Bidi controls that make text render deceptively. Stripped before display.
pub fn strip_bidi_controls(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !matches!(
                *c,
                '\u{202A}' // LRE
                | '\u{202B}' // RLE
                | '\u{202C}' // PDF
                | '\u{202D}' // LRO
                | '\u{202E}' // RLO
                | '\u{2066}' // LRI
                | '\u{2067}' // RLI
                | '\u{2068}' // FSI
                | '\u{2069}' // PDI
                | '\u{200E}' // LRM
                | '\u{200F}' // RLM
                | '\u{061C}' // ALM
            )
        })
        .collect()
}

/// Scrub bidi controls from `title` attributes in place. Native tooltips
/// stay native. Payload is ASCII only (`\uXXXX` escapes), kept in sync with
/// [`strip_bidi_controls`].
pub fn title_sanitize_js() -> String {
    r#"(function() {
  if (window.__ferriteTitleSanitized) return;
  window.__ferriteTitleSanitized = true;
  var RE = /[\u202A-\u202E\u2066-\u2069\u200E\u200F\u061C]/g;
  function scrub(root) {
    try {
      var scope = root || document;
      var els = scope.querySelectorAll ? scope.querySelectorAll("[title]") : [];
      for (var i = 0; i < els.length; i++) {
        var cur = els[i].getAttribute("title");
        var next = cur != null ? String(cur).replace(RE, "") : null;
        if (next != null && next !== cur) els[i].setAttribute("title", next);
      }
    } catch (e) {}
  }
  try { scrub(document); } catch (e) {}
  try {
    var obs = new MutationObserver(function(muts) {
      for (var i = 0; i < muts.length; i++) {
        var m = muts[i];
        if (m.type === "attributes" && m.attributeName === "title") {
          try {
            var t = m.target, c = t.getAttribute("title");
            var n = c != null ? String(c).replace(RE, "") : null;
            if (n != null && n !== c) t.setAttribute("title", n);
          } catch (e) {}
        } else {
          for (var j = 0; j < m.addedNodes.length; j++) {
            var nd = m.addedNodes[j];
            if (nd.nodeType !== 1) continue;
            try {
              if (nd.hasAttribute && nd.hasAttribute("title")) {
                var c2 = nd.getAttribute("title");
                var nx = c2 != null ? String(c2).replace(RE, "") : null;
                if (nx != null && nx !== c2) nd.setAttribute("title", nx);
              }
            } catch (e) {}
            scrub(nd);
          }
        }
      }
    });
    obs.observe(document.documentElement || document, { childList: true, subtree: true, attributes: true, attributeFilter: ["title"] });
  } catch (e) {}
})();"#
        .to_string()
}

pub fn all_js(cosmetic_selectors: &[&str]) -> String {
    let mut out = String::new();
    out.push_str(&fingerprint_spoof_js());
    out.push('\n');
    out.push_str(&param_stripping_js());
    out.push('\n');
    out.push_str(&deamp_js());
    out.push('\n');
    out.push_str(&title_sanitize_js());
    if !cosmetic_selectors.is_empty() {
        let css = cosmetic_css(cosmetic_selectors);
        out.push('\n');
        out.push_str(&cosmetic_js(&css));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_stripping_contains_gclid() {
        let js = param_stripping_js();
        assert!(js.contains("gclid"));
        assert!(js.contains("history.replaceState"));
    }

    #[test]
    fn strip_tracking_params_removes_only_listed() {
        assert_eq!(
            strip_tracking_params("https://x.test/p?utm_source=t&q=keep&fbclid=z"),
            "https://x.test/p?q=keep"
        );
        // Nothing listed: byte-identical, no re-serialization drift.
        assert_eq!(
            strip_tracking_params("https://x.test/p?q=keep"),
            "https://x.test/p?q=keep"
        );
        // Unparseable input passes through.
        assert_eq!(strip_tracking_params("not a url"), "not a url");
        assert_eq!(
            strip_tracking_params("https://x.test/p?gclid=a"),
            "https://x.test/p"
        );
    }

    #[test]
    fn deamp_contains_canonical() {
        let js = deamp_js();
        assert!(js.contains("canonical"));
        assert!(js.contains("amp.project.net"));
    }

    #[test]
    fn cosmetic_css_joins() {
        let css = cosmetic_css(&["div#ad", ".sponsor"]);
        assert_eq!(css, "div#ad, .sponsor { display: none !important; }");
        assert!(cosmetic_css(&[]).is_empty());
    }

    #[test]
    fn cosmetic_js_injects_style() {
        let js = cosmetic_js("div#ad { display: none !important; }");
        assert!(js.contains("createElement(\"style\")"));
        assert!(js.contains("MutationObserver"));
    }

    #[test]
    fn form_risk_reports_password_posts_cross_host() {
        let js = form_risk_js();
        assert!(js.contains("form_risk"));
        assert!(js.contains("input[type=\"password\"]"));
        assert!(js.contains("__ferriteFormRisk"));
        assert!(!js.contains("preventDefault"));
    }

    #[test]
    fn cosmetic_query_asks_once_over_http() {
        let js = cosmetic_query_js();
        // Once-guard per document, http(s) gate, host bridge post.
        assert!(js.contains("__ferriteCosmeticAsked"));
        assert!(js.contains("http:") && js.contains("https:"));
        assert!(js.contains("cosmetic_query"));
        assert!(js.contains("window.location.href"));
        assert!(!js.contains("display: none"));
    }

    #[test]
    fn fingerprint_contains_overrides() {
        let js = fingerprint_spoof_js();
        assert!(js.contains("toDataURL"));
        assert!(js.contains("getImageData"));
        assert!(js.contains("getParameter"));
        assert!(js.contains("createAnalyser"));
    }

    #[test]
    fn strip_bidi_controls_removes_overrides() {
        assert_eq!(
            strip_bidi_controls("invoice_\u{202E}gpj.exe"),
            "invoice_gpj.exe"
        );
        assert_eq!(strip_bidi_controls("plain title"), "plain title");
        assert_eq!(strip_bidi_controls("a\u{2066}b\u{2069}c"), "abc");
        assert_eq!(strip_bidi_controls("x\u{200E}y\u{061C}z"), "xyz");
        assert_eq!(strip_bidi_controls(""), "");
    }

    #[test]
    fn title_sanitize_js_covers_bidi_set() {
        let js = title_sanitize_js();
        assert!(js.contains("__ferriteTitleSanitized"));
        assert!(js.contains("MutationObserver"));
        assert!(js.contains("[title]"));
        assert!(js.contains("attributeFilter"));
        for esc in [
            "\\u202A-\\u202E",
            "\\u2066-\\u2069",
            "\\u200E",
            "\\u200F",
            "\\u061C",
        ] {
            assert!(js.contains(esc), "JS payload missing {esc}");
        }
    }

    #[test]
    fn all_js_combines() {
        let js = all_js(&["div#ad"]);
        assert!(js.contains("toDataURL"));
        assert!(js.contains("history.replaceState"));
        assert!(js.contains("canonical"));
        assert!(js.contains("display: none"));
        assert!(js.contains("__ferriteTitleSanitized"));
    }
}
