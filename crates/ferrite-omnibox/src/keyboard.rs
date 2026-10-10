//! Keyboard hints for webview navigation.

/// Hint alphabet. Short letter combos.
pub const DEFAULT_CHARSET: &str = "asdfghjkl";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintTheme {
    pub accent_hex: String,
    pub text_hex: String,
    pub font_family_css: String,
    pub font_face_style: String,
}

impl HintTheme {
    pub fn iris() -> Self {
        Self {
            accent_hex: "#4F46E5".to_owned(),
            text_hex: "#020203".to_owned(),
            font_family_css: "'Geist Pixel Square',monospace".to_owned(),
            font_face_style: geist_font_face_css(),
        }
    }

    pub fn gold() -> Self {
        Self {
            accent_hex: "#F0C75E".to_owned(),
            ..Self::iris()
        }
    }
}

pub fn geist_font_face_css() -> String {
    "@font-face{font-family:'Geist Pixel Square';src:url('ferrite://assets/geist-pixel-square.ttf') format('truetype');font-weight:normal;font-style:normal;}".to_owned()
}

/// Overlay hint badges on clickable elements.
pub fn hint_generation_js(charset: &str, theme: &HintTheme) -> String {
    let charset = if charset.trim().is_empty() {
        DEFAULT_CHARSET
    } else {
        charset
    };
    // Charset is filtered to letters only.
    let escaped: String = charset
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .collect();
    let charset_js = if escaped.is_empty() {
        DEFAULT_CHARSET.to_string()
    } else {
        escaped
    };

    format!(
        r#"(function() {{
  if (window.__ferriteHintsActive) return;
  window.__ferriteHintsActive = true;
  const charset = "{charset}";
  const candidates = Array.from(document.querySelectorAll('a, button, input[type="submit"], [role="button"]'));
  const visible = candidates.filter(el => {{
    const r = el.getBoundingClientRect();
    const style = window.getComputedStyle(el);
    return r.width > 0 && r.height > 0 && style.display !== "none" && style.visibility !== "hidden" && style.opacity !== "0";
  }});
  function comboFor(n) {{
    let s = "";
    let base = charset.length;
    let v = n;
    do {{ s = charset[v % base] + s; v = Math.floor(v / base) - 1; }} while (v >= 0);
    return s;
  }}
  const overlay = document.createElement("div");
  overlay.id = "__ferrite-hints";
  overlay.style.cssText = "position:fixed;inset:0;pointer-events:none;z-index:2147483647;";
  const st = document.createElement("style");
  st.textContent = "{face}";
  document.documentElement.appendChild(st);
  document.documentElement.appendChild(overlay);
  const hintMap = new Map();
  visible.forEach((el, idx) => {{
    const r = el.getBoundingClientRect();
    const hint = comboFor(idx);
    const badge = document.createElement("div");
    badge.textContent = hint;
    badge.dataset.hint = hint;
    badge.style.cssText = "position:absolute;left:" + (r.left+window.scrollX) + "px;top:" + (r.top+window.scrollY) + "px;background:{accent};color:{text};font:11px {family};padding:1px 3px;border-radius:3px;pointer-events:none;z-index:2147483647;";
    overlay.appendChild(badge);
    hintMap.set(hint, el);
  }});
  let buffer = "";
  function onKey(e) {{
    if (e.key === "Escape") {{ cleanup(); return; }}
    if (e.key.length === 1) {{
      buffer += e.key.toLowerCase();
      let exact = hintMap.get(buffer);
      if (exact) {{
        exact.click();
        cleanup();
        return;
      }}
      let any = Array.from(hintMap.keys()).some(k => k.startsWith(buffer));
      if (!any) {{ buffer = ""; }}
      overlay.querySelectorAll("div").forEach(b => {{
        b.style.opacity = b.dataset.hint.startsWith(buffer) ? "1" : "0.2";
      }});
    }}
  }}
  function cleanup() {{
    document.removeEventListener("keydown", onKey, true);
    st.remove();
    overlay.remove();
    window.__ferriteHintsActive = false;
  }}
  document.addEventListener("keydown", onKey, true);
}})();"#,
        charset = charset_js,
        accent = theme.accent_hex,
        text = theme.text_hex,
        family = theme.font_family_css,
        face = theme.font_face_style
    )
}

pub fn hint_cancel_js() -> &'static str {
    r#"(function(){ const o=document.getElementById("__ferrite-hints"); if(o) o.remove(); window.__ferriteHintsActive=false; })();"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_js_with_charset() {
        let js = hint_generation_js("ab", &HintTheme::iris());
        assert!(js.contains(r#"const charset = "ab""#));
        assert!(js.contains("querySelectorAll"));
        assert!(js.contains("__ferrite-hints"));
    }

    #[test]
    fn hint_theming_is_parameterized_not_hardcoded() {
        let sentinel = crate::keyboard::HintTheme {
            accent_hex: "#ABCDEF".to_owned(),
            text_hex: "#123456".to_owned(),
            ..HintTheme::iris()
        };
        let js = hint_generation_js(DEFAULT_CHARSET, &sentinel);
        assert!(js.contains("#ABCDEF"), "accent param must flow through");
        assert!(js.contains("#123456"), "text param must flow through");
        assert!(!js.contains("#4F46E5"), "no hardcoded iris in template");
        assert!(!js.contains("#E5E5E5"), "no legacy chrome text");

        let iris = hint_generation_js(DEFAULT_CHARSET, &HintTheme::iris());
        assert!(iris.contains("#4F46E5"));
        assert!(iris.contains("#020203"));
        assert!(iris.contains(&HintTheme::iris().font_family_css));
        assert!(iris.contains(&HintTheme::iris().font_face_style));
    }

    #[test]
    fn gold_context_swaps_accent_only() {
        let iris = hint_generation_js(DEFAULT_CHARSET, &HintTheme::iris());
        let gold = hint_generation_js(DEFAULT_CHARSET, &HintTheme::gold());
        assert!(gold.contains("#F0C75E"));
        assert!(iris.contains("#4F46E5"));
        assert!(!iris.contains("#F0C75E"), "standard must not leak gold");
        for marker in [r#"const charset = "asdfghjkl""#, "__ferrite-hints"] {
            assert!(iris.contains(marker) && gold.contains(marker));
        }
    }

    #[test]
    fn font_face_points_at_asset_route() {
        let css = geist_font_face_css();
        assert!(css.contains("@font-face"));
        assert!(css.contains("'Geist Pixel Square'"));
        assert!(css.contains("ferrite://assets/geist-pixel-square.ttf"));
        assert!(css.contains("truetype"));
    }

    #[test]
    fn filters_invisible() {
        let js = hint_generation_js("asdf", &HintTheme::iris());
        assert!(js.contains("display !== \"none\""));
        assert!(js.contains("getBoundingClientRect"));
    }

    #[test]
    fn cancel_js_removes_overlay() {
        let js = hint_cancel_js();
        assert!(js.contains("__ferrite-hints"));
        assert!(js.contains("remove()"));
    }

    #[test]
    fn empty_charset_falls_back() {
        let js = hint_generation_js("", &HintTheme::iris());
        assert!(js.contains(DEFAULT_CHARSET));
    }

    #[test]
    fn hint_js_is_substantial() {
        assert!(hint_generation_js(DEFAULT_CHARSET, &HintTheme::iris()).len() > 800);
    }
}
