//! JS payloads for content handling.

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

pub fn all_js(cosmetic_selectors: &[&str]) -> String {
    let mut out = String::new();
    out.push_str(&fingerprint_spoof_js());
    out.push('\n');
    out.push_str(&param_stripping_js());
    out.push('\n');
    out.push_str(&deamp_js());
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
    fn fingerprint_contains_overrides() {
        let js = fingerprint_spoof_js();
        assert!(js.contains("toDataURL"));
        assert!(js.contains("getImageData"));
        assert!(js.contains("getParameter"));
        assert!(js.contains("createAnalyser"));
    }

    #[test]
    fn all_js_combines() {
        let js = all_js(&["div#ad"]);
        assert!(js.contains("toDataURL"));
        assert!(js.contains("history.replaceState"));
        assert!(js.contains("canonical"));
        assert!(js.contains("display: none"));
    }
}
