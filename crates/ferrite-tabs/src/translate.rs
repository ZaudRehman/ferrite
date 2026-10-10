//! On device translation via engine models. No page text leaves the device.
//! Script does the work, Rust owns settings, trigger, and state validation.

pub const DEFAULT_TARGET: &str = "en";

/// Max chunks per run (1000 chars each). Beyond this stops with a note.
pub const MAX_CHUNKS: usize = 400;

/// Max source chars per chunk. Matches the script splitter.
pub const MAX_CHUNK_CHARS: usize = 1000;

/// Max selection chars per run.
pub const MAX_SELECTION_CHARS: usize = 20_000;

/// Marker in `ferrite://reader` where the script inlines at serve time.
pub const TRANSLATE_MARKER: &str = "/*__FERRITE_TRANSLATE__*/";

/// Persisted as `translate_enabled` and `translate_target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslateSettings {
    pub enabled: bool,
    pub target: String,
}

impl Default for TranslateSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            target: DEFAULT_TARGET.to_owned(),
        }
    }
}

impl TranslateSettings {
    /// Read from the settings JSON envelope (missing keys keep defaults;
    /// an invalid target falls back to [`DEFAULT_TARGET`], never empty).
    pub fn load_from_json(v: &serde_json::Value) -> Self {
        let enabled = v
            .get("translate_enabled")
            .and_then(|x| x.as_bool())
            .unwrap_or(true);
        let target = v
            .get("translate_target")
            .and_then(|x| x.as_str())
            .map(normalize_target)
            .unwrap_or_else(|| DEFAULT_TARGET.to_owned());
        Self { enabled, target }
    }
}

/// Validate a BCP47-ish language tag (`en`, `en-US`, `zh-Hant`, `es-419`).
/// Returns the canonical trimmed input, or [`DEFAULT_TARGET`] when invalid.
pub fn normalize_target(raw: &str) -> String {
    let t = raw.trim();
    if t.len() > 12 || !is_lang_tag(t) {
        return DEFAULT_TARGET.to_owned();
    }
    t.to_owned()
}

/// Validate a page-reported language tag (reporter posts are
/// page-supplied and untrusted). Underscores become hyphens (`pt_BR`),
/// comma lists take the first entry, overlong input is refused.
pub fn valid_page_lang(raw: &str) -> Option<String> {
    let t = raw.trim().replace('_', "-");
    let first = t.split(',').next().unwrap_or("").trim();
    if first.is_empty() || first.len() > 12 || !is_lang_tag(first) {
        return None;
    }
    Some(first.to_owned())
}

/// Primary subtag, lowercased (`pt-BR` → `pt`). Comparison unit for the
/// translate offer: regional variants of the target never offer.
pub fn primary_lang(tag: &str) -> String {
    tag.split('-').next().unwrap_or("").to_lowercase()
}

fn is_lang_tag(t: &str) -> bool {
    let mut parts = t.split('-');
    let primary = parts.next().unwrap_or("");
    if !(2..=3).contains(&primary.len()) || !primary.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    for p in parts {
        let ok_script = p.len() == 4 && p.chars().all(|c| c.is_ascii_alphabetic());
        let ok_region = (p.len() == 2 && p.chars().all(|c| c.is_ascii_alphabetic()))
            || p.len() == 3 && p.chars().all(|c| c.is_ascii_digit());
        if !(ok_script || ok_region) {
            return false;
        }
    }
    true
}

/// What the trigger translates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslateMode {
    Selection,
    Page,
}

impl TranslateMode {
    fn as_str(self) -> &'static str {
        match self {
            TranslateMode::Selection => "selection",
            TranslateMode::Page => "page",
        }
    }
}

/// Build the `__ferriteTranslate` invocation for a tab. The target is
/// re-normalized here, so a forged settings value can never inject script:
/// the emitted call carries only an enum mode and a validated tag.
pub fn translate_eval(mode: TranslateMode, target: &str) -> String {
    let target = normalize_target(target);
    let payload = serde_json::json!({"mode": mode.as_str(), "target": target});
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_owned());
    format!("__ferriteTranslate({json})")
}

/// Terminal/in-flight states posted by the script as `translate_state`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslateStatus {
    Translating,
    Done,
    Restored,
    Unavailable,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslateState {
    pub status: TranslateStatus,
    pub done: u32,
    pub total: u32,
    pub message: String,
}

impl TranslateState {
    /// Validate a page-supplied `translate_state` post. Unknown statuses,
    /// absurd counters, and overlong messages are rejected (the channel is
    /// shared with page JS, so every field is untrusted).
    pub fn parse(v: &serde_json::Value) -> Option<Self> {
        let status = match v.get("status").and_then(|x| x.as_str())? {
            "translating" => TranslateStatus::Translating,
            "done" => TranslateStatus::Done,
            "restored" => TranslateStatus::Restored,
            "unavailable" => TranslateStatus::Unavailable,
            "error" => TranslateStatus::Error,
            _ => return None,
        };
        let clamp = |key: &str| {
            v.get(key)
                .and_then(|x| x.as_u64())
                .map(|n| n.min(1_000_000) as u32)
                .unwrap_or(0)
        };
        let message: String = v
            .get("message")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .chars()
            .take(200)
            .collect();
        Some(Self {
            status,
            done: clamp("done"),
            total: clamp("total"),
            message,
        })
    }

    /// Terminal states worth surfacing as a toast (progress itself stays
    /// in the page badge; a background tab never toasts, enforced by the
    /// shell gating on the active tab).
    pub fn toast_text(&self) -> Option<String> {
        match self.status {
            TranslateStatus::Unavailable => {
                Some("Translation is not available on this device.".to_owned())
            }
            TranslateStatus::Error if !self.message.is_empty() => Some(self.message.clone()),
            TranslateStatus::Error => Some("Translation failed.".to_owned()),
            _ => None,
        }
    }
}

/// On-device translation content script (document start, reader inlined).
/// Feature-detects per run, queues paragraphs with progress, 400-chunk cap.
/// Selection renders to a panel, page mode replaces in place. No cloud.
pub fn translate_js() -> &'static str {
    r##"(function(){
  if (window.__ferriteTranslateInstalled) return;
  window.__ferriteTranslateInstalled = true;
  var MAX_CHUNK = 1000, MAX_RUN = 400, MAX_SEL = 20000;
  var session = null;
  function post(o){ try{ window.ipc.postMessage(JSON.stringify(o)); }catch(e){} }
  function state(status, done, total, message){
    post({type:'translate_state', status:status, done:done||0, total:total||0, message:message||''});
  }
  function validTag(t){
    return typeof t === 'string' && /^[A-Za-z]{2,3}(-[A-Za-z]{4})?(-([A-Za-z]{2}|\d{3}))?$/.test(t);
  }
  function primary(t){ return String(t||'').split('-')[0].toLowerCase(); }
  function pageLang(){
    var l = '';
    try{ l = String(document.documentElement.getAttribute('lang')||''); }catch(e){}
    l = l.replace(/_/g,'-').split(',')[0].trim();
    return validTag(l) ? l : '';
  }
  async function detectSource(sample){
    try{
      if('LanguageDetector' in self && self.LanguageDetector && self.LanguageDetector.create){
        var d = await self.LanguageDetector.create();
        var r = await d.detect(String(sample).slice(0, 2000));
        if(r && r.length && r[0].confidence >= 0.5 && validTag(r[0].detectedLanguage)) return r[0].detectedLanguage;
      }
    }catch(e){}
    return pageLang();
  }
  function badgeRoot(){
    var b = document.getElementById('ferriteTranslateBadge');
    if(b) return b;
    b = document.createElement('div');
    b.id = 'ferriteTranslateBadge';
    b.setAttribute('role','status');
    b.setAttribute('aria-live','polite');
    b.setAttribute('data-ferrite-notranslate','');
    b.style.cssText = 'position:fixed;right:16px;bottom:16px;z-index:2147483647;max-width:min(420px,90vw);background:#1a1a1a;color:#e8e6e3;border:1px solid #6b6b6b;border-radius:8px;padding:12px 14px;font:13px/1.5 system-ui,sans-serif;box-shadow:0 4px 24px rgba(0,0,0,.4);';
    try{ document.documentElement.appendChild(b); }catch(e){ return null; }
    return b;
  }
  function badgeNote(text){
    var b = badgeRoot(); if(!b) return;
    b.textContent = '';
    var s = document.createElement('div'); s.textContent = text; b.appendChild(s);
    var c = document.createElement('button'); c.textContent = 'Dismiss';
    c.style.cssText = 'margin-top:8px;background:transparent;border:1px solid #9a978f;color:#e8e6e3;border-radius:4px;padding:4px 12px;cursor:pointer;font:inherit;';
    c.addEventListener('click', function(){ closeBadge(); });
    b.appendChild(c);
  }
  function badgeProgress(done, total){
    var b = badgeRoot(); if(!b) return;
    b.textContent = '';
    var s = document.createElement('div');
    s.textContent = 'Translating: ' + done + ' of ' + total + ' passages';
    b.appendChild(s);
  }
  function badgeResult(text, onRestore){
    var b = badgeRoot(); if(!b) return;
    b.textContent = '';
    var done = document.createElement('div'); done.textContent = 'Translation complete.'; b.appendChild(done);
    var pre = document.createElement('div');
    pre.textContent = text;
    pre.style.cssText = 'max-height:240px;overflow:auto;margin:8px 0;padding:8px;border:1px solid #6b6b6b;border-radius:4px;white-space:pre-wrap;';
    b.appendChild(pre);
    var row = document.createElement('div');
    var copy = document.createElement('button'); copy.textContent = 'Copy';
    copy.style.cssText = 'margin-right:8px;background:transparent;border:1px solid #9a978f;color:#e8e6e3;border-radius:4px;padding:4px 12px;cursor:pointer;font:inherit;';
    copy.addEventListener('click', function(){
      var done2 = function(ok){ copy.textContent = ok ? 'Copied' : 'Copy failed'; };
      try{
        if(navigator.clipboard && navigator.clipboard.writeText){
          navigator.clipboard.writeText(text).then(function(){ done2(true); }, function(){ done2(false); });
        } else { done2(false); }
      }catch(e){ done2(false); }
    });
    row.appendChild(copy);
    var close = document.createElement('button'); close.textContent = onRestore ? 'Show original' : 'Dismiss';
    close.style.cssText = 'background:transparent;border:1px solid #9a978f;color:#e8e6e3;border-radius:4px;padding:4px 12px;cursor:pointer;font:inherit;';
    close.addEventListener('click', function(){ if(onRestore){ onRestore(); } closeBadge(); });
    row.appendChild(close);
    b.appendChild(row);
  }
  function closeBadge(){
    try{ var b = document.getElementById('ferriteTranslateBadge'); if(b && b.parentNode) b.parentNode.removeChild(b); }catch(e){}
  }
  function splitChunks(text){
    var out = [];
    var paras = String(text).split(/\n{2,}|\r\n{2,}/);
    for(var i = 0; i < paras.length; i++){
      var p = paras[i].trim();
      if(!p) continue;
      while(p.length > MAX_CHUNK){
        var cut = -1;
        var m = p.slice(0, MAX_CHUNK + 1).match(/.*[.?!]["']?\s/);
        if(m) cut = m[0].length;
        else {
          var sp = p.slice(0, MAX_CHUNK).lastIndexOf(' ');
          cut = sp > MAX_CHUNK / 2 ? sp : MAX_CHUNK;
        }
        out.push(p.slice(0, cut).trim());
        p = p.slice(cut).trim();
      }
      if(p) out.push(p);
    }
    return out;
  }
  async function ensureTranslator(source, target){
    var avail = '';
    try{
      if(self.Translator && self.Translator.availability){
        avail = await self.Translator.availability({sourceLanguage: source, targetLanguage: target});
      }
    }catch(e){ avail = ''; }
    if(avail === 'unavailable'){
      state('unavailable', 0, 0, '');
      badgeNote('Translation is not available on this device.');
      return null;
    }
    try{
      var t = await self.Translator.create({sourceLanguage: source, targetLanguage: target,
        monitor: function(m){
          if(m && m.addEventListener){
            m.addEventListener('downloadprogress', function(){ badgeProgress(0, 1); });
          }
        }});
      return t;
    }catch(e){
      state('error', 0, 0, 'Could not start translation.');
      badgeNote('Could not start translation.');
      return null;
    }
  }
  function collectPageNodes(){
    var out = [];
    var skip = {SCRIPT:1, STYLE:1, NOSCRIPT:1, TEXTAREA:1, INPUT:1, CODE:1, PRE:1, SELECT:1, OPTION:1, BUTTON:1};
    try{
      if(!document.body) return out;
      var w = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, {acceptNode: function(n){
        var p = n.parentElement;
        if(!p || skip[p.tagName]) return NodeFilter.FILTER_REJECT;
        if(p.closest && p.closest('[data-ferrite-notranslate]')) return NodeFilter.FILTER_REJECT;
        if(!n.nodeValue || !/[^\s]/.test(n.nodeValue)) return NodeFilter.FILTER_REJECT;
        return NodeFilter.FILTER_ACCEPT;
      }});
      var n;
      while((n = w.nextNode())) out.push(n);
    }catch(e){}
    return out;
  }
  function restore(){
    if(!session) return;
    try{
      for(var i = 0; i < session.orig.length; i++){
        try{ session.orig[i].node.nodeValue = session.orig[i].text; }catch(e){}
      }
    }catch(e){}
    session = null;
    closeBadge();
    state('restored', 0, 0, '');
  }
  async function runPage(target){
    if(session){ restore(); return; }
    if(!('Translator' in self) || !self.Translator || !self.Translator.create){
      state('unavailable', 0, 0, '');
      badgeNote('Translation is not available on this device.');
      return;
    }
    var nodes = collectPageNodes();
    if(!nodes.length){ badgeNote('Nothing to translate on this page.'); return; }
    var sample = '';
    for(var s = 0; s < nodes.length && sample.length < 2000; s++){ sample += ' ' + nodes[s].nodeValue; }
    var source = await detectSource(sample);
    if(!source){
      state('error', 0, 0, 'Could not detect the page language.');
      badgeNote('Could not detect the page language.');
      return;
    }
    if(primary(source) === primary(target)){
      badgeNote('This page already appears to be in the target language.');
      return;
    }
    var t = await ensureTranslator(source, target);
    if(!t) return;
    var orig = [];
    for(var o = 0; o < nodes.length; o++){ orig.push({node: nodes[o], text: nodes[o].nodeValue}); }
    session = {translator: t, orig: orig};
    var total = Math.min(nodes.length, MAX_RUN);
    var truncated = nodes.length > MAX_RUN;
    state('translating', 0, total, '');
    badgeProgress(0, total);
    for(var i = 0; i < total; i++){
      var seg = splitChunks(nodes[i].nodeValue);
      var parts = [];
      var failed = false;
      for(var k = 0; k < seg.length; k++){
        try{ parts.push(await t.translate(seg[k])); }
        catch(e){ failed = true; break; }
      }
      if(failed){
        state('error', i, total, 'Translation stopped partway.');
        badgeNote('Translation stopped partway; translated passages are kept. Invoke again to restore the original.');
        return;
      }
      try{ nodes[i].nodeValue = parts.join(' '); }catch(e){}
      state('translating', i + 1, total, '');
      badgeProgress(i + 1, total);
    }
    state('done', total, total, truncated ? 'Translated the first ' + total + ' passages.' : '');
    badgeResult(truncated ? 'Translated the first ' + total + ' passages.' : 'Page translated.', function(){ restore(); });
  }
  async function runSelection(target){
    closeBadge();
    var text = '';
    try{ text = String(window.getSelection ? window.getSelection().toString() : ''); }catch(e){}
    text = text.trim();
    if(!text){ badgeNote('Select text first, then translate.'); return; }
    if(text.length > MAX_SEL) text = text.slice(0, MAX_SEL);
    if(!('Translator' in self) || !self.Translator || !self.Translator.create){
      state('unavailable', 0, 0, '');
      badgeNote('Translation is not available on this device.');
      return;
    }
    var source = await detectSource(text);
    if(!source){
      state('error', 0, 0, 'Could not detect the selection language.');
      badgeNote('Could not detect the selection language.');
      return;
    }
    if(primary(source) === primary(target)){
      badgeNote('This text already appears to be in the target language.');
      return;
    }
    var t = await ensureTranslator(source, target);
    if(!t) return;
    var chunks = splitChunks(text).slice(0, MAX_RUN);
    state('translating', 0, chunks.length, '');
    badgeProgress(0, chunks.length);
    var parts = [];
    for(var i = 0; i < chunks.length; i++){
      try{ parts.push(await t.translate(chunks[i])); }
      catch(e){
        state('error', i, chunks.length, 'Translation stopped partway.');
        badgeNote('Translation stopped partway.');
        return;
      }
      state('translating', i + 1, chunks.length, '');
      badgeProgress(i + 1, chunks.length);
    }
    state('done', chunks.length, chunks.length, '');
    badgeResult(parts.join('\n\n'), null);
  }
  window.__ferriteTranslate = function(opts){
    try{
      opts = opts || {};
      var target = validTag(opts.target) ? opts.target : 'en';
      if(opts.mode === 'selection'){ runSelection(target); }
      else { runPage(target); }
    }catch(e){
      state('error', 0, 0, 'Translation failed.');
    }
  };
})();"##
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_validation_accepts_bcp47_tags() {
        for good in ["en", "de", "fr", "en-US", "pt-BR", "zh-Hant", "es-419"] {
            assert_eq!(normalize_target(good), good, "should accept {good}");
        }
    }

    #[test]
    fn target_validation_rejects_garbage() {
        for bad in [
            "",
            "e",
            "engl",
            "en_us",
            "en--US",
            "en-U",
            "123",
            "en-US-extra-long-tag",
            "javascript:alert(1)",
            "en; rm -rf",
        ] {
            assert_eq!(normalize_target(bad), DEFAULT_TARGET, "should reject {bad}");
        }
        assert_eq!(normalize_target("  "), DEFAULT_TARGET);
    }

    #[test]
    fn page_lang_validation_trusts_nothing() {
        assert_eq!(valid_page_lang("de"), Some("de".to_owned()));
        assert_eq!(valid_page_lang("pt_BR"), Some("pt-BR".to_owned()));
        assert_eq!(valid_page_lang("zh-Hant, en"), Some("zh-Hant".to_owned()));
        assert_eq!(valid_page_lang(""), None);
        assert_eq!(valid_page_lang("x"), None);
        assert_eq!(valid_page_lang("javascript:alert(1)"), None);
        assert_eq!(valid_page_lang("en--US"), None);
        assert_eq!(valid_page_lang("en-US-extra-long-tag"), None);
        assert_eq!(primary_lang("pt-BR"), "pt");
        assert_eq!(primary_lang("EN"), "en");
        assert_eq!(primary_lang("zh-Hant"), "zh");
    }

    #[test]
    fn eval_carries_only_mode_and_validated_target() {
        let eval = translate_eval(TranslateMode::Page, "de");
        assert!(eval.starts_with("__ferriteTranslate("), "entry call");
        assert!(eval.contains("\"mode\":\"page\""), "mode");
        assert!(eval.contains("\"target\":\"de\""), "target");
        let hostile = translate_eval(TranslateMode::Selection, "\"); evil();//");
        assert!(
            hostile.contains(&format!("\"target\":\"{DEFAULT_TARGET}\"")),
            "hostile target falls back, got {hostile}"
        );
        assert!(!hostile.contains("evil"), "no injection");
    }

    #[test]
    fn state_parsing_bounds_untrusted_fields() {
        let v = serde_json::json!({"status": "translating", "done": 3, "total": 10});
        let s = TranslateState::parse(&v).expect("valid");
        assert_eq!(s.done, 3);
        assert_eq!(s.total, 10);
        assert!(TranslateState::parse(&serde_json::json!({"status": "nope"})).is_none());
        let huge = serde_json::json!({"status": "done", "done": 99999999999u64, "total": 5});
        assert_eq!(TranslateState::parse(&huge).expect("valid").done, 1_000_000);
        let long = serde_json::json!({"status": "error", "message": "x".repeat(500)});
        assert_eq!(
            TranslateState::parse(&long).expect("valid").message.len(),
            200
        );
    }

    #[test]
    fn state_toasts_only_terminal_honest_states() {
        let un =
            TranslateState::parse(&serde_json::json!({"status": "unavailable"})).expect("valid");
        assert_eq!(
            un.toast_text(),
            Some("Translation is not available on this device.".to_owned())
        );
        let prog = TranslateState::parse(
            &serde_json::json!({"status": "translating", "done": 1, "total": 9}),
        )
        .expect("valid");
        assert_eq!(prog.toast_text(), None, "progress stays in-page");
        let done = TranslateState::parse(&serde_json::json!({"status": "done"})).expect("valid");
        assert_eq!(done.toast_text(), None, "completion stays in-page");
    }

    #[test]
    fn script_contract_markers() {
        let js = translate_js();
        assert!(js.contains("'Translator' in self"), "feature detect");
        assert!(js.contains("LanguageDetector"), "detector companion");
        assert!(js.contains("await t.translate"), "sequential queue");
        assert!(
            js.contains("Translation is not available on this device."),
            "honest fallback"
        );
        assert!(js.contains("__ferriteTranslate"), "entry point");
        assert!(!js.contains("pack"), "never inventories packs");
        assert!(!js.contains("http"), "no network fallback");
        assert!(!js.contains("</"), "reader-inline safe");
    }

    #[test]
    fn settings_load_defaults_and_fallbacks() {
        let empty = serde_json::json!({});
        let s = TranslateSettings::load_from_json(&empty);
        assert!(s.enabled);
        assert_eq!(s.target, DEFAULT_TARGET);
        let off = serde_json::json!({"translate_enabled": false, "translate_target": "fr"});
        let s = TranslateSettings::load_from_json(&off);
        assert!(!s.enabled);
        assert_eq!(s.target, "fr");
        let bad = serde_json::json!({"translate_target": "!!"});
        assert_eq!(
            TranslateSettings::load_from_json(&bad).target,
            DEFAULT_TARGET
        );
    }
}
