//! PiP candidate scoring plus page scripts. The floating window is engine
//! owned. Ferrite picks which video and wires the button. Ad hosts demote
//! before largest playing fallback.

use serde::{Deserialize, Serialize};

/// Min area for eligibility. Below this are thumbnails and stubs.
pub const PIP_MIN_AREA: u32 = 120 * 90;
/// Min edge: narrow strips are not content.
pub const PIP_MIN_EDGE: u32 = 60;
/// Max candidates per IPC message. Fits the 64KB cap.
pub const PIP_MAX_CANDIDATES: usize = 8;
/// Max source chars per candidate. Same rule as titles.
pub const PIP_MAX_SRC_CHARS: usize = 512;

/// One page reported video candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipCandidate {
    #[serde(default)]
    pub src: String,
    #[serde(default)]
    pub hint: String,
    #[serde(default)]
    pub area: u32,
    #[serde(default)]
    pub playing: bool,
    #[serde(default)]
    pub audible: bool,
}

/// Page reported PiP status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipReport {
    #[serde(default)]
    pub eligible: bool,
    /// A PiP window is currently held by this document.
    #[serde(default)]
    pub active: bool,
    /// Scored candidates, best first, capped at [`PIP_MAX_CANDIDATES`].
    #[serde(default)]
    pub candidates: Vec<PipCandidate>,
}

/// Substrings marking ad-adjacent video slots (preroll stubs, overlays).
const AD_HINTS: &[&str] = &[
    "ad-",
    "ads-",
    "advert",
    "preroll",
    "midroll",
    "postroll",
    "sponsor",
    "promo",
    "banner",
    "overlay-ad",
    "outstream",
    "companion",
];

/// True when the element's id/class/name blob looks ad-adjacent.
pub fn is_ad_hint(hint: &str) -> bool {
    let h = hint.to_lowercase();
    AD_HINTS.iter().any(|k| h.contains(k))
}

/// Host of a candidate source, lowercased, without port/userinfo.
pub fn candidate_host(src: &str) -> String {
    let after_scheme = src.split("://").nth(1).unwrap_or(src);
    let host_port = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let host = host_port.split('@').next_back().unwrap_or(host_port);
    let host = host.split(':').next().unwrap_or(host);
    host.trim().trim_matches('.').to_lowercase()
}

/// True when `host` equals or nests under a known tracker host.
pub fn host_is_trackerish(host: &str, tracker_hosts: &[String]) -> bool {
    let h = host.trim().to_lowercase();
    if h.is_empty() {
        return false;
    }
    tracker_hosts.iter().any(|t| {
        let t = t.trim().to_lowercase();
        !t.is_empty() && (h == t || h.ends_with(&format!(".{t}")))
    })
}

/// Score a candidate: playing plus large plus audible wins. Ad hints and
/// tracker-hosted sources demote hard, never veto, so a tracker-hosted
/// feature still PiPs when it is the only video.
pub fn score_candidate(c: &PipCandidate, tracker_hosts: &[String]) -> i64 {
    let mut score: i64 = 0;
    if c.playing {
        // Playing dominates: a paused fullscreen poster never beats a
        // playing video (PiP of a still frame is pointless). The bonus
        // exceeds the max area term so playing is the primary key.
        score += 5_000_000;
    }
    score += (c.area as i64).min(4_000_000);
    if c.audible {
        score += 50_000;
    }
    if is_ad_hint(&c.hint) {
        score -= 800_000;
    }
    if host_is_trackerish(&candidate_host(&c.src), tracker_hosts) {
        score -= 500_000;
    }
    score
}

/// Pick the best candidate index (ad-aware, largest-playing wins).
/// Returns `None` for an empty list.
pub fn pick_candidate(candidates: &[PipCandidate], tracker_hosts: &[String]) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .max_by_key(|(_, c)| score_candidate(c, tracker_hosts))
        .map(|(i, _)| i)
}

/// Parse and sanitize a page-reported `pip` envelope. Oversize lists
/// truncate, overlong sources char-truncate. Never fails on shape drift
/// (unknown ignored, missing reads as ineligible or inactive).
pub fn parse_report(v: &serde_json::Value) -> PipReport {
    let mut report = PipReport {
        eligible: false,
        active: false,
        candidates: Vec::new(),
    };
    if let Ok(parsed) = serde_json::from_value::<PipReport>(v.clone()) {
        report.eligible = parsed.eligible;
        report.active = parsed.active;
        report.candidates = parsed
            .candidates
            .into_iter()
            .take(PIP_MAX_CANDIDATES)
            .map(|mut c| {
                c.src = c.src.chars().take(PIP_MAX_SRC_CHARS).collect();
                c.hint = c.hint.chars().take(256).collect();
                c
            })
            .collect();
    }
    // Eligibility is recomputed Rust-side from the candidates so a lying or
    // stale page cannot light the chrome button without a real candidate.
    // A page with no candidates can still hold a PiP window it opened
    // itself (page-initiated path), `active` is trusted as reported.
    if report.candidates.is_empty() {
        report.eligible = false;
    } else {
        report.eligible = report.candidates.iter().any(|c| c.playing);
    }
    report
}

/// Shared candidate-collection snippet: visible, sized videos with their
/// metadata. `TRACKERS` is substituted by the caller when known.
const COLLECT_SNIPPET: &str = r#"
function __ferritePipCollect(){
  var out=[];
  try{
    var vids=document.querySelectorAll('video');
    for(var i=0;i<vids.length && out.length<8;i++){
      var v=vids[i];
      try{
        var r=v.getBoundingClientRect();
        var w=Math.round(r.width)||v.clientWidth||v.videoWidth||0;
        var h=Math.round(r.height)||v.clientHeight||v.videoHeight||0;
        var area=w*h;
        if(w<60||h<60||area<10800) continue;
        if(r.bottom<0||r.right<0||r.top>(window.innerHeight||0)||r.left>(window.innerWidth||0)) continue;
        var src='';
        try{ src=v.currentSrc||v.src||''; }catch(e){}
        if(!src){ try{ var s=v.querySelector('source[src]'); if(s) src=s.src||''; }catch(e){} }
        var hint='';
        try{ hint=((v.id||'')+' '+(v.className||'')+' '+(v.getAttribute('name')||'')).toLowerCase(); }catch(e){}
        var playing=false;
        try{ playing=!v.paused&&!v.ended&&v.readyState>0; }catch(e){}
        var audible=false;
        try{ audible=!!(v.mozHasAudio||(v.webkitAudioDecodedByteCount&&v.webkitAudioDecodedByteCount>0)||(v.audioTracks&&v.audioTracks.length>0)); }catch(e){}
        out.push({src:String(src).slice(0,512),hint:String(hint).slice(0,256),area:area,playing:!!playing,audible:!!audible});
      }catch(e){}
    }
  }catch(e){}
  return out;
}
function __ferritePipAdHint(h){
  h=String(h||'').toLowerCase();
  return ['ad-','ads-','advert','preroll','midroll','postroll','sponsor','promo','banner','overlay-ad','outstream','companion'].some(function(k){return h.indexOf(k)>=0;});
}
function __ferritePipTrackerish(src){
  try{
    var host=String(src||'').split('://')[1]||String(src||'');
    host=host.split('/')[0].split('?')[0].split('#')[0].split('@').pop().split(':')[0].replace(/^\.+|\.+$/g,'').toLowerCase();
    if(!host) return false;
    return TRACKERS.some(function(t){ t=String(t).toLowerCase(); return host===t||host.slice(-t.length-1)==='.'+t; });
  }catch(e){ return false; }
}
function __ferritePipPick(cands){
  var best=-1,bestScore=-1e18;
  for(var i=0;i<cands.length;i++){
    var c=cands[i],s=c.area;
    if(c.playing) s+=5000000;
    if(c.audible) s+=50000;
    if(__ferritePipAdHint(c.hint)) s-=800000;
    if(__ferritePipTrackerish(c.src)) s-=500000;
    if(s>bestScore){bestScore=s;best=i;}
  }
  return best;
}
"#;

/// Content-script reporter: posts `{type:'pip', eligible, active,
/// candidates}` on video play/pause/end events, PiP enter/leave, and DOM
/// mutations (bounded: at most one post per 500ms).
pub fn pip_report_js() -> String {
    let collect = COLLECT_SNIPPET.replace("TRACKERS", "[]");
    format!(
        r#"(function(){{
  if (window.__ferritePipInstalled) return;
  window.__ferritePipInstalled = true;
  var last = 0;
  function snapshot() {{
    var cands = __ferritePipCollect();
    var eligible = cands.some(function(c){{ return c.playing; }});
    var active = false;
    try {{ active = !!document.pictureInPictureElement; }} catch(e) {{}}
    return {{eligible: eligible, active: active, candidates: cands}};
  }}
  function report() {{
    var now = Date.now();
    if (now - last < 500) return;
    last = now;
    try {{ window.ipc.postMessage(JSON.stringify(Object.assign({{type:'pip'}}, snapshot()))); }} catch(e) {{}}
  }}
  try {{
    document.addEventListener('play', report, true);
    document.addEventListener('pause', report, true);
    document.addEventListener('ended', report, true);
    document.addEventListener('enterpictureinpicture', report, true);
    document.addEventListener('leavepictureinpicture', report, true);
  }} catch(e) {{}}
  try {{
    var obs = new MutationObserver(function(muts){{
      for (var i = 0; i < muts.length; i++) {{
        var nodes = muts[i].addedNodes;
        for (var j = 0; j < nodes.length; j++) {{
          var n = nodes[j];
          if (n && n.nodeType === 1) {{
            if ((n.matches && n.matches('video,source')) || (n.querySelectorAll && n.querySelector('video'))) {{ report(); return; }}
          }}
        }}
      }}
    }});
    obs.observe(document.documentElement, {{childList:true, subtree:true}});
  }} catch(e) {{}}
  report();
}})();
{collect}"#,
        collect = collect
    )
}

/// Build the chrome-button enter script: picks the best playing video
/// (ad-aware, largest wins), tries Document PiP first, then element PiP.
/// `tracker_hosts` embeds as a literal so scoring reuses the blocklist.
/// Resolves silently: failures post `{type:'pip_error'}` instead of throwing.
pub fn pip_enter_js(tracker_hosts: &[String]) -> String {
    let trackers = serde_json::to_string(tracker_hosts).unwrap_or_else(|_| "[]".into());
    let collect = COLLECT_SNIPPET.replace("TRACKERS", &trackers);
    format!(
        r#"(function(){{
async function __ferritePipEnter(){{
  var cands = __ferritePipCollect();
  if (!cands.length) {{
    try {{ window.ipc.postMessage(JSON.stringify({{type:'pip_error', reason:'no-video'}})); }} catch(e) {{}}
    return;
  }}
  var idx = __ferritePipPick(cands);
  var vids = Array.prototype.slice.call(document.querySelectorAll('video'));
  var sized = vids.filter(function(v){{
    try {{
      var r = v.getBoundingClientRect();
      var w = Math.round(r.width) || v.clientWidth || 0;
      var h = Math.round(r.height) || v.clientHeight || 0;
      return w >= 60 && h >= 60 && (w * h) >= 10800;
    }} catch(e) {{ return false; }}
  }});
  var v = sized[idx] || sized[0] || vids[0];
  if (!v) {{
    try {{ window.ipc.postMessage(JSON.stringify({{type:'pip_error', reason:'no-video'}})); }} catch(e) {{}}
    return;
  }}
  try {{ await v.play().catch(function(){{}}); }} catch(e) {{}}
  try {{
    if (window.documentPictureInPicture && documentPictureInPicture.requestWindow) {{
      var win = await documentPictureInPicture.requestWindow({{width: Math.min(640, v.clientWidth || 640), height: Math.min(360, v.clientHeight || 360)}});
      try {{
        var style = win.document.createElement('style');
        style.textContent = 'html,body{{margin:0;background:#000;height:100%}}video{{width:100%;height:100%}}';
        win.document.head.appendChild(style);
        var clone = v.cloneNode(true);
        clone.setAttribute('autoplay','');
        try {{ clone.src = v.currentSrc || v.src; }} catch(e) {{}}
        win.document.body.appendChild(clone);
        try {{ await clone.play().catch(function(){{}}); }} catch(e) {{}}
        var close = function(){{ try {{ win.close(); }} catch(e) {{}} }};
        v.addEventListener('ended', close, {{once:true}});
        try {{ window.ipc.postMessage(JSON.stringify({{type:'pip', eligible:true, active:true, candidates:[]}})); }} catch(e) {{}}
        return;
      }} catch(e) {{
        try {{ win.close(); }} catch(x) {{}}
      }}
    }}
  }} catch(e) {{}}
  try {{
    if (document.pictureInPictureElement && document.pictureInPictureElement !== v) {{
      try {{ await document.exitPictureInPicture(); }} catch(e) {{}}
    }}
    await v.requestPictureInPicture();
  }} catch(e) {{
    try {{ window.ipc.postMessage(JSON.stringify({{type:'pip_error', reason:String((e && e.name) || 'failed')}})); }} catch(x) {{}}
  }}
}}
__ferritePipEnter();
}})();
{collect}"#,
        collect = collect
    )
}

/// Exit any held PiP window (element PiP plus Document PiP). The
/// Document-PiP window close is guarded (existence check first,
/// fail-soft), the element exit is a safe no-op when nothing is held.
pub fn pip_exit_js() -> &'static str {
    r#"(function(){
  (async function(){
    try { if (window.documentPictureInPicture && documentPictureInPicture.window) documentPictureInPicture.window.close(); } catch(e) {}
    try { if (document.pictureInPictureElement) await document.exitPictureInPicture(); } catch(e) {}
  })();
})();"#
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(src: &str, hint: &str, area: u32, playing: bool, audible: bool) -> PipCandidate {
        PipCandidate {
            src: src.into(),
            hint: hint.into(),
            area,
            playing,
            audible,
        }
    }

    #[test]
    fn largest_playing_wins() {
        let cs = vec![
            cand("https://cdn.test/small.mp4", "", 50_000, true, false),
            cand("https://cdn.test/big.mp4", "", 800_000, true, true),
        ];
        assert_eq!(pick_candidate(&cs, &[]), Some(1));
    }

    #[test]
    fn playing_beats_larger_paused() {
        let cs = vec![
            cand("https://cdn.test/big.mp4", "", 2_000_000, false, false),
            cand("https://cdn.test/small.mp4", "", 100_000, true, false),
        ];
        assert_eq!(pick_candidate(&cs, &[]), Some(1));
    }

    #[test]
    fn ad_hint_is_demoted_not_vetoed() {
        let cs = vec![
            cand(
                "https://ads.test/preroll.mp4",
                "preroll ad-slot",
                900_000,
                true,
                true,
            ),
            cand("https://cdn.test/film.mp4", "feature", 400_000, true, true),
        ];
        assert_eq!(pick_candidate(&cs, &[]), Some(1));
        // Alone, the ad still plays, demote, never veto.
        assert_eq!(pick_candidate(&cs[..1], &[]), Some(0));
    }

    #[test]
    fn tracker_host_is_demoted() {
        let trackers = vec!["tracker.test".to_owned()];
        let cs = vec![
            cand("https://tracker.test/v.mp4", "", 900_000, true, false),
            cand("https://cdn.test/v.mp4", "", 400_000, true, false),
        ];
        assert_eq!(pick_candidate(&cs, &trackers), Some(1));
        assert!(host_is_trackerish("sub.tracker.test", &trackers));
        assert!(!host_is_trackerish("tracker.test.evil.org", &trackers));
        assert!(!host_is_trackerish("", &trackers));
    }

    #[test]
    fn empty_picks_nothing() {
        let empty: Vec<PipCandidate> = Vec::new();
        assert_eq!(pick_candidate(&empty, &[]), None);
    }

    #[test]
    fn report_recomputes_eligibility_and_caps() {
        let v = serde_json::json!({
            "eligible": true,
            "active": false,
            "candidates": (0..20).map(|i| serde_json::json!({
                "src": format!("https://cdn.test/{i}.mp4"),
                "hint": "",
                "area": 200_000,
                "playing": false,
                "audible": false,
            })).collect::<Vec<_>>(),
        });
        let r = parse_report(&v);
        assert!(!r.eligible, "paused candidates must not light the button");
        assert_eq!(r.candidates.len(), PIP_MAX_CANDIDATES);
        // Page-initiated PiP with no candidates keeps its active flag.
        let v = serde_json::json!({"eligible": false, "active": true});
        let r = parse_report(&v);
        assert!(r.active);
        assert!(!r.eligible);
    }

    #[test]
    fn report_truncates_long_sources() {
        let v = serde_json::json!({
            "eligible": true,
            "active": false,
            "candidates": [{"src": "https://cdn.test/".to_string() + &"x".repeat(2000), "hint": "", "area": 200_000, "playing": true, "audible": false}],
        });
        let r = parse_report(&v);
        assert!(r.eligible);
        assert!(r.candidates[0].src.chars().count() <= PIP_MAX_SRC_CHARS);
    }

    #[test]
    fn report_survives_shape_drift() {
        let r = parse_report(&serde_json::json!({"nonsense": 1}));
        assert!(!r.eligible && !r.active && r.candidates.is_empty());
    }

    #[test]
    fn enter_script_covers_both_pip_apis() {
        let js = pip_enter_js(&["tracker.test".to_owned()]);
        assert!(js.contains("requestPictureInPicture"));
        assert!(js.contains("documentPictureInPicture"));
        assert!(js.contains("tracker.test"), "blocklist signal embedded");
        assert!(js.contains("pip_error"), "failures report, never throw");
    }

    #[test]
    fn exit_script_closes_document_pip_guarded() {
        let js = pip_exit_js();
        assert!(js.contains("exitPictureInPicture"), "element exit kept");
        assert!(js.contains("documentPictureInPicture"), "doc PiP closed");
        assert!(
            js.contains("documentPictureInPicture.window"),
            "window close"
        );
        assert!(js.contains(".close()"), "close call");
        assert!(
            js.contains("window.documentPictureInPicture"),
            "existence guard first, fail-soft"
        );
    }

    #[test]
    fn reporter_posts_bounded_pip_envelope() {
        let js = pip_report_js();
        assert!(js.contains("\"type:'pip'") || js.contains("type:'pip'"));
        assert!(js.contains("pictureInPictureElement"));
        assert!(js.contains("MutationObserver"));
    }

    #[test]
    fn candidate_host_parsing() {
        assert_eq!(candidate_host("https://cdn.test:8080/a.mp4"), "cdn.test");
        assert_eq!(
            candidate_host("https://user:pass@sub.cdn.test/x"),
            "sub.cdn.test"
        );
        assert_eq!(candidate_host("https://TRACKER.test/x"), "tracker.test");
    }
}
