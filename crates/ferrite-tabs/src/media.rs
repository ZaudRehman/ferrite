//! Media truth: DRM failure surfacing and capability reporting. Codecs and
//! decode stay engine owned. No cast hook exists in pinned wry, so none
//! ships. No GPU toggle: it would risk the proxy args.

pub const WIDEVINE_ID: &str = "com.widevine.alpha";
pub const PLAYREADY_IDS: &[&str] = &[
    "com.microsoft.playready",
    "com.microsoft.playready.recommendation",
];
/// Apple key family prefix.
pub const FAIRPLAY_PREFIX: &str = "com.apple.fps";

/// Max reports kept for toast dedupe. Bounds hostile pages.
pub const DRM_DEDUPE_CAP: usize = 64;

/// Playable set per platform. Unconfirmed reads as weak, never assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrmCapabilities {
    /// PlayReady-gated content plays.
    pub playready: bool,
    /// Widevine-gated content (Netflix, Spotify, most premium streaming).
    pub widevine: bool,
}

impl DrmCapabilities {
    /// Compile-time capability truth for this build target.
    pub const fn current() -> Self {
        #[cfg(target_os = "windows")]
        {
            Self {
                playready: true,
                widevine: false,
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            Self {
                playready: false,
                widevine: false,
            }
        }
    }

    /// Plain-language consequence for a blocked key system. Widevine gets
    /// the specific structural explanation; anything else gets the generic
    /// honest variant (never claims a cause the module did not observe).
    pub fn blocked_message(system: &str) -> String {
        if system == WIDEVINE_ID {
            "This site needs Widevine DRM, which Ferrite does not provide. \
             PlayReady-protected and unencrypted media play normally."
                .to_owned()
        } else if system.is_empty() || system == "unknown" {
            "This protected video could not play. Ferrite does not provide \
             Widevine DRM, which most premium streaming services require."
                .to_owned()
        } else {
            format!(
                "This site requested DRM playback ({system}) that is not \
                 available in Ferrite. Unencrypted media plays normally."
            )
        }
    }
}

/// Validated `media_drm_blocked` post. Every field is page-supplied and
/// untrusted: the system id is length-capped (never allowlisted, the
/// message shaper only special-cases exact Widevine), the host is capped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaDrmReport {
    /// Requested key system id, or `"unknown"` for black-box player failure.
    pub system: String,
}

impl MediaDrmReport {
    pub fn parse(v: &serde_json::Value) -> Option<Self> {
        let system: String = v
            .get("system")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
            .chars()
            .take(80)
            .collect();
        if system.is_empty() {
            return None;
        }
        Some(Self { system })
    }

    /// Dedupe key: one toast per tab, host, and system per session.
    pub fn dedupe_key(&self, tab: &str, host: &str) -> String {
        let host: String = host.chars().take(253).collect();
        format!("{tab}\0{host}\0{}", self.system)
    }
}

/// EME failure monitor (document start, main frame only). Call-through only,
/// never alters page behavior. Denials post once per system per document,
/// media errors post `unknown`. ClearKey never reports (it always works).
pub fn media_monitor_js() -> &'static str {
    r##"(function(){
  if (window.__ferriteMediaInstalled) return;
  window.__ferriteMediaInstalled = true;
  var reported = {};
  function post(o){ try{ window.ipc.postMessage(JSON.stringify(o)); }catch(e){} }
  function report(system){
    system = String(system || 'unknown').slice(0, 80) || 'unknown';
    if(reported[system]) return;
    reported[system] = true;
    post({type:'media_drm_blocked', system:system});
  }
  function isDrmSystem(ks){
    ks = String(ks || '');
    if(ks === 'com.widevine.alpha') return true;
    if(ks === 'com.microsoft.playready' || ks === 'com.microsoft.playready.recommendation') return true;
    if(ks === 'com.apple.fps' || ks.indexOf('com.apple.fps.') === 0) return true;
    return false;
  }
  try{
    var proto = (window.Navigator && window.Navigator.prototype) ? window.Navigator.prototype : null;
    var holder = null;
    try{
      if(proto && typeof proto.requestMediaKeySystemAccess === 'function') holder = {obj: proto, key: 'requestMediaKeySystemAccess'};
      else if(typeof navigator.requestMediaKeySystemAccess === 'function') holder = {obj: navigator, key: 'requestMediaKeySystemAccess'};
    }catch(e){}
    if(holder){
      var orig = holder.obj[holder.key];
      var wrapped = function(){
        var ks = (arguments.length > 0) ? arguments[0] : '';
        var r;
        try{ r = orig.apply(this, arguments); }
        catch(e){
          if(isDrmSystem(ks)) report(ks);
          throw e;
        }
        try{
          if(r && typeof r.catch === 'function'){
            r.catch(function(e){
              try{
                if(e && (e.name === 'NotSupportedError' || e.name === 'SecurityError') && isDrmSystem(ks)) report(ks);
              }catch(_){}
            });
          }
        }catch(_){}
        return r;
      };
      try{ holder.obj[holder.key] = wrapped; }catch(e){}
    }
  }catch(e){}
  try{
    var armed = (typeof WeakSet === 'function') ? new WeakSet() : null;
    var seen = [];
    function isArmed(el){
      if(armed) return armed.has(el);
      return seen.indexOf(el) !== -1;
    }
    function arm(el){
      if(armed){ armed.add(el); return; }
      if(seen.length < 256 && seen.indexOf(el) === -1) seen.push(el);
    }
    document.addEventListener('encrypted', function(e){
      try{ if(e && e.target) arm(e.target); }catch(_){}
    }, true);
    document.addEventListener('error', function(e){
      try{
        var t = e ? e.target : null;
        if(!t || (t.tagName !== 'VIDEO' && t.tagName !== 'AUDIO')) return;
        if(!isArmed(t)) return;
        var err = t.error;
        if(err && err.code === 4) report('unknown');
      }catch(_){}
    }, true);
  }catch(e){}
})();"##
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_capabilities_name_the_gap() {
        #[cfg(target_os = "windows")]
        {
            let caps = DrmCapabilities::current();
            assert!(caps.playready, "PlayReady is native to WebView2");
            assert!(!caps.widevine, "Widevine has no WebView2 support");
        }
        let msg = DrmCapabilities::blocked_message(WIDEVINE_ID);
        assert!(msg.contains("Widevine"), "names the cause");
        assert!(msg.contains("PlayReady"), "states what works");
        let generic = DrmCapabilities::blocked_message("unknown");
        assert!(generic.contains("Widevine"), "names the likely cause");
        assert!(
            !generic.contains("requires DRM support Ferrite"),
            "no vagueness"
        );
    }

    #[test]
    fn blocked_message_never_echoes_arbitrary_systems_verbatim_as_cause() {
        let msg = DrmCapabilities::blocked_message("com.evil.fingerprint");
        assert!(msg.contains("com.evil.fingerprint"), "echoes for context");
        assert!(!msg.contains("does not provide com.evil"), "no false claim");
    }

    #[test]
    fn report_parsing_bounds_untrusted_fields() {
        let v = serde_json::json!({"system": "com.widevine.alpha"});
        let r = MediaDrmReport::parse(&v).expect("valid");
        assert_eq!(r.system, WIDEVINE_ID);
        assert_eq!(
            MediaDrmReport::parse(&serde_json::json!({}))
                .expect("valid")
                .system,
            "unknown"
        );
        assert!(MediaDrmReport::parse(&serde_json::json!({"system": ""})).is_none());
        let long = serde_json::json!({"system": "x".repeat(500)});
        assert_eq!(
            MediaDrmReport::parse(&long).expect("valid").system.len(),
            80
        );
        let k = r.dedupe_key("tab-1", "netflix.com");
        assert!(k.contains("tab-1") && k.contains("netflix.com") && k.contains(WIDEVINE_ID));
    }

    #[test]
    fn script_contract_markers() {
        let js = media_monitor_js();
        assert!(js.contains("requestMediaKeySystemAccess"), "EME tripwire");
        assert!(js.contains("com.widevine.alpha"), "names the gap");
        assert!(js.contains("media_drm_blocked"), "report channel");
        assert!(js.contains("NotSupportedError"), "denial matcher");
        assert!(
            js.contains("MEDIA_ERR_SRC_NOT_SUPPORTED") || js.contains("err.code === 4"),
            "error pairing"
        );
        assert!(!js.contains("org.w3.clearkey"), "clearkey never reported");
        assert!(!js.contains("innerHTML"), "no HTML surface");
        assert!(!js.contains("</"), "inline safe");
        assert!(!js.contains("http"), "no network");
    }
}
