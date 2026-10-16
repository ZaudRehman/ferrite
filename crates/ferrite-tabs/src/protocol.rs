//! `ferrite://` protocol handler.

use std::borrow::Cow;

use http::{Request, Response};

/// Vendored extraction stack, evaluated on demand in the content tab only.
pub const READABILITY_JS: &str = include_str!("../assets/Readability.js");
pub const PURIFY_JS: &str = include_str!("../assets/purify.min.js");

/// Extract with Readability, sanitize with DOMPurify, post back chunked.
/// Unsanitized HTML never leaves this closure.
pub const READER_DRIVER_JS: &str = r#"(function(){
  function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}
  try{
    var R = window.__ferriteReadability;
    if(!R){ post({type:'reader_result',ok:false}); return; }
    var article = null;
    try{ article = new R(document.cloneNode(true),{}).parse(); }catch(e){ article=null; }
    if(!article || !article.content){ post({type:'reader_result',ok:false}); return; }
    var clean = '';
    try{ clean = DOMPurify.sanitize(article.content,{USE_PROFILES:{html:true}}); }catch(e){ post({type:'reader_result',ok:false}); return; }
    if(!clean){ post({type:'reader_result',ok:false}); return; }
    var CH = 24000, n = Math.max(1, Math.ceil(clean.length/CH));
    post({type:'reader_result',ok:true,title:article.title||'',byline:article.byline||'',site:article.siteName||'',length:article.length||0,excerpt:article.excerpt||'',chunks:n});
    for(var i=0;i<n;i++) post({type:'reader_chunk',seq:i,total:n,html:clean.slice(i*CH,(i+1)*CH)});
  }catch(e){ post({type:'reader_result',ok:false}); }
})();"#;

/// Full evaluate payload: libs (installed once per document) + driver.
/// ~120KB of parse on an explicit gesture, never per-navigation.
pub fn reader_extract_js() -> String {
    let mut out = String::with_capacity(READABILITY_JS.len() + PURIFY_JS.len() + 4096);
    out.push_str("(function(){if(!window.__ferriteReadability){var module={exports:{}};");
    out.push_str(READABILITY_JS);
    out.push_str(";window.__ferriteReadability=module.exports;");
    out.push('}');
    out.push_str(READER_DRIVER_JS);
    out.push_str("})();");
    out
}

pub const NEWTAB_HTML: &str = r##"<!DOCTYPE html><!-- ferrite://newtab -->
<html><head><meta charset="utf-8"><title>New Tab - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;min-height:100%;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
.wrap{max-width:640px;margin:0 auto;padding:48px 24px 64px;display:flex;flex-direction:column;align-items:center}
.skip{position:absolute;left:-9999px;top:8px;background:var(--f-surface-1);border:1px solid var(--f-accent);color:var(--f-fg);padding:8px 12px;border-radius:var(--pk-ctl-radius,6px);font-size:13px;z-index:10}
.skip:focus{left:8px}
.logo{font-size:28px;font-weight:800;letter-spacing:-0.02em;color:var(--f-fg);margin:0 0 8px}
.sub{color:var(--f-muted);font-size:13px;margin:0 0 24px;letter-spacing:0.08em;text-transform:uppercase;font-family:var(--f-font-pixel)}
.search{width:100%;max-width:560px}
.search input{width:100%;box-sizing:border-box;padding:16px;font-size:15px;border:1px solid var(--f-border);border-radius:var(--pk-pill-radius,16px);background:var(--f-surface-1);color:var(--f-fg);outline:none;font-family:var(--f-font-mono)}
.search input:focus{border-color:var(--f-accent)}
section{width:100%;max-width:560px;margin-top:32px}
section h2{font-size:11px;letter-spacing:.08em;text-transform:uppercase;color:var(--f-faint);font-weight:400;margin:0 0 12px;font-family:var(--f-font-pixel)}
.shortcuts{display:grid;grid-template-columns:repeat(4,1fr);gap:12px}
.tile{background:var(--f-surface-1);border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);display:flex;flex-direction:column;align-items:center;gap:6px;cursor:pointer;padding:14px 6px 10px;position:relative;min-width:0}
.tile:hover,.tile:focus{border-color:var(--f-accent);outline:none}
.tile .av{width:32px;height:32px;border-radius:50%;background:var(--f-surface-2);color:var(--f-muted);font-size:14px;font-weight:700;display:flex;align-items:center;justify-content:center}
.tile .t{font-size:11px;color:var(--f-muted);max-width:100%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.tile .ops{position:absolute;top:2px;right:2px;display:none;gap:2px}
.tile:hover .ops,.tile:focus-within .ops{display:flex}
.tile .ops button{background:var(--f-surface-2);border:1px solid var(--f-border);color:var(--f-muted);font-size:10px;border-radius:var(--pk-ctl-radius,6px);padding:1px 5px;cursor:pointer;font-family:inherit}
.tile .ops button:hover{color:var(--f-fg);border-color:var(--f-accent)}
.tile.add{border-style:dashed;cursor:pointer}
.tile.add .av{background:none;font-size:20px;font-weight:400;color:var(--f-faint)}
.tile.add:hover .av{color:var(--f-accent)}
#dlgback{position:fixed;inset:0;background:rgba(0,0,0,.55);display:flex;align-items:flex-start;justify-content:center;padding-top:18vh;z-index:20}
#dlgback[hidden]{display:none}
#dlg{background:var(--f-surface-1);border:1px solid var(--f-border);border-radius:var(--pk-pop-radius,4px);padding:20px 22px;width:min(420px,90vw)}
#dlg h2{font-size:15px;margin:0 0 12px;font-weight:600}
#dlg label{display:block;font-size:12px;color:var(--f-muted);margin:10px 0 2px}
#dlg input{width:100%;box-sizing:border-box;background:var(--f-bg);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-ctl-radius,6px);padding:9px 10px;font-size:13px;font-family:var(--f-font-mono)}
#dlg input:focus{border-color:var(--f-accent);outline:none}
#derr{color:var(--f-danger);font-size:12px;min-height:18px;margin:8px 0 0}
.dacts{display:flex;justify-content:flex-end;gap:8px;margin-top:12px}
.dacts button{background:var(--f-surface-2);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-ctl-radius,6px);padding:8px 16px;cursor:pointer;font-size:13px;font-family:inherit}
.dacts button:hover{border-color:var(--f-accent)}
.dacts button.primary{background:var(--f-accent);border-color:var(--f-accent);color:var(--f-bg);font-weight:700}
.dash{display:flex;gap:24px;background:var(--f-surface-1);border:0.5px solid var(--f-border);padding:14px 18px}
.dash .m .n{font-size:20px;font-weight:700;font-variant-numeric:tabular-nums}
.dash .m .l{font-size:11px;color:var(--f-muted);margin-top:2px}
.reading{display:flex;flex-direction:column;gap:2px}
.reading .r{display:flex;gap:10px;align-items:baseline;padding:8px 10px;border-radius:var(--pk-card-radius,8px);cursor:pointer}
.reading .r:hover{background:var(--f-surface-1)}
.reading .r .t{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:13px}
.reading .r .u{color:var(--f-faint);font-size:11px;max-width:40%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;flex:none}
.empty{color:var(--f-faint);font-size:12px;padding:4px 10px}
.guestbar{display:flex;gap:10px;align-items:center;width:100%;max-width:560px;box-sizing:border-box;background:var(--f-surface-1);border:1px solid var(--f-warning);border-radius:var(--pk-card-radius,8px);padding:10px 14px;font-size:12px;color:var(--f-fg);margin:0 0 16px}
.guestbar[hidden]{display:none}
.guestbar b{color:var(--f-warning)}
.privbar{display:flex;gap:10px;align-items:center;width:100%;max-width:560px;box-sizing:border-box;background:var(--f-surface-1);border:1px solid var(--f-warning);border-radius:var(--pk-card-radius,8px);padding:10px 14px;font-size:12px;color:var(--f-fg);margin:0 0 16px}
.privbar[hidden]{display:none}
.privbar b{color:var(--f-warning)}
</style></head>
<body>
<div class="wrap">
<a class="skip" id="skip" href="#q">Skip to search</a>
<p class="logo">Ferrite</p>
<p class="sub">Sovereign Browser</p>
<div class="guestbar" id="guestbar" hidden><span><b>Incognito session.</b> Nothing is saved: history, passwords and downloads vanish when you close.</span></div>
<div class="privbar" id="privbar" hidden><span><b>Private tab.</b> Nothing from this tab is saved: close it and it never happened.</span></div>
<div class="search" role="search">
<input type="text" id="q" placeholder="Search or enter address" autocomplete="off" spellcheck="false">
</div>
<section aria-label="Top sites">
<h2>Top sites</h2>
<div class="shortcuts" id="tiles" role="navigation" aria-label="Top sites"></div>
</section>
<div id="dlgback" hidden><div id="dlg" role="dialog" aria-modal="true" aria-labelledby="dlgh">
<h2 id="dlgh">Add shortcut</h2>
<label for="du">URL</label><input id="du" type="text" placeholder="https://…" autocomplete="off" spellcheck="false">
<label for="dt">Name</label><input id="dt" type="text" placeholder="Label" autocomplete="off" spellcheck="false">
<p id="derr" aria-live="polite"></p>
<div class="dacts"><button id="dcancel">Cancel</button><button id="dsave" class="primary">Save</button></div>
</div></div>
<section aria-label="Blocked requests" id="dashsec">
<h2>Blocked</h2>
<div class="dash" id="dash"></div>
</section>
<section aria-label="Continue reading" id="readsec">
<h2>Continue reading</h2>
<div class="reading" id="reading"></div>
</section>
</div>
<script>
(function(){
  var q=document.getElementById('q'), tiles=document.getElementById('tiles'),
      dash=document.getElementById('dash'), reading=document.getElementById('reading'),
      readsec=document.getElementById('readsec');
  function post(o){ try{ window.ipc.postMessage(JSON.stringify(o)); }catch(e){} }
  function esc(s){ return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;'); }
  function avatarOf(text){ var m=String(text||'').match(/[A-Za-z0-9]/); return m?m[0].toUpperCase():'•'; }
  function labelOf(t){ return t.title||t.url||''; }
  function nav(url){ if(url) post({type:'navigate',url:url}); }
  window.__ferriteNewTab=function(d){
    d=d||{};
    var gb=document.getElementById('guestbar');
    if(gb) gb.hidden=!d.guest;
    // Private tabs in user mode get their own banner (same ephemeral
    // promise, tab scope instead of window scope). Never both at once:
    // an incognito window has no private tabs, only ephemeral everything.
    var pb=document.getElementById('privbar');
    if(pb) pb.hidden=!(d.private&&!d.guest);
    // Tiles: frecency order from Rust, pins first. Letter avatars by
    // design here (tile rows carry no icon bytes to keep pushes small);
    // the tab strip and its hover card render engine-reported icons.
    var h='';
    (d.tiles||[]).forEach(function(t){
      var label=labelOf(t);
      h+='<div class="tile" tabindex="0" data-go="'+esc(t.url)+'" title="'+esc(label)+'&#10;'+esc(t.url)+'">'
        +'<span class="av">'+esc(avatarOf(label))+'</span><span class="t">'+esc(label)+'</span>'
        +'<span class="ops"><button data-edit="'+esc(t.url)+'" data-title="'+esc(t.title||'')+'" title="Edit">✎</button>'
        +(t.pinned
          ?'<button data-unpin="'+esc(t.url)+'" title="Unpin">Unpin</button>'
          :'<button data-pin="'+esc(t.url)+'" data-title="'+esc(t.title||'')+'" title="Pin">Pin</button>'
          +'<button data-del="'+esc(t.url)+'" title="Hide from top sites">×</button>')+'</span></div>';
    });
    if(!(d.tiles||[]).length) h+='<div class="empty">Browse and your top sites land here.</div>';
    h+='<div class="tile add" tabindex="0" data-add title="Add shortcut"><span class="av">+</span><span class="t">Add shortcut</span></div>';
    tiles.innerHTML=h;
    // Dashboard: raw verdict counts (today / trailing 7d / all time).
    // Headline only, no "time/data saved" estimates, ever.
    var s=d.stats||{today:0,week:0,all:0};
    dash.innerHTML='<div class="m"><div class="n">'+s.today+'</div><div class="l">blocked today</div></div>'
      +'<div class="m"><div class="n">'+s.week+'</div><div class="l">last 7 days</div></div>'
      +'<div class="m"><div class="n">'+s.all+'</div><div class="l">all time</div></div>';
    // Continue reading: unread list, newest first.
    var rh='';
    (d.reading||[]).forEach(function(r){
      rh+='<div class="r" tabindex="0" data-go="'+esc(r.url)+'"><span class="t">'+esc(r.title||r.url)+'</span><span class="u">'+esc(r.url)+'</span></div>';
    });
    reading.innerHTML=rh;
    readsec.style.display=rh?'':'none';
  };
  function goFrom(el){
    var row=el.closest?el.closest('[data-go]'):null;
    if(row&&row.getAttribute('data-go')) nav(row.getAttribute('data-go'));
  }
  document.addEventListener('click',function(e){
    var pin=e.target.closest?e.target.closest('[data-pin]'):null;
    if(pin){ e.stopPropagation(); post({type:'ntp_pin',url:pin.getAttribute('data-pin'),title:pin.getAttribute('data-title')||''}); return; }
    var unpin=e.target.closest?e.target.closest('[data-unpin]'):null;
    if(unpin){ e.stopPropagation(); post({type:'ntp_unpin',url:unpin.getAttribute('data-unpin')}); return; }
    var edit=e.target.closest?e.target.closest('[data-edit]'):null;
    if(edit){ e.stopPropagation(); openDlg(edit.getAttribute('data-edit'),edit.getAttribute('data-title')||''); return; }
    var del=e.target.closest?e.target.closest('[data-del]'):null;
    if(del){ e.stopPropagation(); post({type:'ntp_remove',url:del.getAttribute('data-del')}); return; }
    var add=e.target.closest?e.target.closest('[data-add]'):null;
    if(add){ e.stopPropagation(); openDlg('',''); return; }
    goFrom(e.target);
  });
  // Shortcut dialog: add via the + tile, edit via the ✎ button on any
  // tile. A URL change is remove-plus-pin (Rust upserts by exact URL, so
  // a rename alone is one pin). http(s) only, validated before posting.
  var dlgback=document.getElementById('dlgback'), du=document.getElementById('du'),
      dt=document.getElementById('dt'), derr=document.getElementById('derr'),
      dlgh=document.getElementById('dlgh'), editing='';
  function openDlg(url,title){
    editing=url||'';
    dlgh.textContent=editing?'Edit shortcut':'Add shortcut';
    du.value=url||''; dt.value=title||''; derr.textContent='';
    dlgback.hidden=false; du.focus(); du.select();
  }
  function closeDlg(){ dlgback.hidden=true; editing=''; }
  document.getElementById('dcancel').addEventListener('click',closeDlg);
  dlgback.addEventListener('click',function(e){ if(e.target===dlgback) closeDlg(); });
  document.addEventListener('keydown',function(e){ if(e.key==='Escape'&&!dlgback.hidden) closeDlg(); });
  function dlgSave(){
    var u=du.value.trim(), t=dt.value.trim();
    if(!/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(u)) u='https://'+u;
    try{
      var p=new URL(u);
      if(p.protocol!=='http:'&&p.protocol!=='https:') throw new Error('scheme');
      if(!p.hostname) throw new Error('host');
    }catch(err){ derr.textContent='Enter a valid http(s) URL.'; du.focus(); du.select(); return; }
    if(editing&&u!==editing) post({type:'ntp_remove',url:editing});
    post({type:'ntp_pin',url:u,title:t||u});
    closeDlg();
  }
  document.getElementById('dsave').addEventListener('click',dlgSave);
  du.addEventListener('keydown',function(e){ if(e.key==='Enter'){ e.preventDefault(); dlgSave(); } });
  dt.addEventListener('keydown',function(e){ if(e.key==='Enter'){ e.preventDefault(); dlgSave(); } });
  // Tile grid arrows (4-column grid); skip link jumps straight to search.
  tiles.addEventListener('keydown',function(e){
    var cells=Array.prototype.slice.call(tiles.querySelectorAll('.tile'));
    var i=cells.indexOf(document.activeElement);
    if(i<0) return;
    var n=null;
    if(e.key==='ArrowRight') n=(i+1)%cells.length;
    else if(e.key==='ArrowLeft') n=(i-1+cells.length)%cells.length;
    else if(e.key==='ArrowDown') n=(i+4)%cells.length;
    else if(e.key==='ArrowUp') n=(i-4+cells.length)%cells.length;
    else if(e.key==='Enter'){
      e.preventDefault();
      var ae=document.activeElement;
      var add=ae&&ae.closest?ae.closest('[data-add]'):null;
      if(add){ openDlg('',''); return; }
      goFrom(ae); return;
    }
    else return;
    e.preventDefault(); cells[n].focus();
  });
  reading.addEventListener('keydown',function(e){
    if(e.key==='Enter'){ e.preventDefault(); goFrom(document.activeElement); }
  });
  document.getElementById('skip').addEventListener('click',function(e){ e.preventDefault(); q.focus(); q.select(); });
  q.addEventListener('keydown',function(e){
    if(e.key==='Enter'){
      var v=q.value.trim();
      // Rust owns classification (URL vs engine search), no local
      // fallback: a timer fallback once hijacked slow classifications.
      if(v) post({type:'search',query:v});
    }
  });
  // No autofocus: the toolbar omnibox holds focus on new tabs (kept).
  document.addEventListener('keydown',function(e){ if((e.ctrlKey||e.metaKey) && e.key.toLowerCase()==='k'){ e.preventDefault(); q.focus(); q.select(); } });
  post({type:'newtab_data'});
})();
</script>
</body></html>
"##;

pub const TABSEARCH_HTML: &str = r#"<!DOCTYPE html><!-- ferrite://tabsearch -->
<html><head><meta charset="utf-8"><title>Command Palette - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;height:100%;background:rgba(0,0,0,.62);font-family:var(--f-font-ui);overflow:hidden}
.backdrop{position:fixed;inset:0}
.panel{width:640px;max-width:calc(100vw - 64px);margin:16vh auto 0;background:var(--f-surface-1);border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);animation:rise 140ms ease-out}
@keyframes rise{from{opacity:0;transform:translateY(-4px)}to{opacity:1;transform:none}}
.eyebrow{display:flex;justify-content:space-between;align-items:center;padding:10px 16px 0;font-family:var(--f-font-mono);font-size:11px;letter-spacing:.06em;text-transform:uppercase;color:var(--f-muted)}
.inputrow{display:flex;align-items:center;gap:10px;padding:10px 16px 12px;border-bottom:1px solid var(--f-border)}
.inputrow svg{flex:none;color:var(--f-muted)}
#q{flex:1;background:transparent;border:0;outline:none;color:var(--f-fg);font-size:17px;font-family:var(--f-font-ui);padding:2px 0;min-width:0}
#q::placeholder{color:var(--f-faint)}
.kbd{font-family:var(--f-font-mono);font-size:11px;color:var(--f-muted);border:1px solid var(--f-border);border-radius:var(--pk-ctl-radius,6px);padding:1px 6px;flex:none}
#results{max-height:336px;overflow-y:auto;padding:4px}
.row{display:flex;align-items:center;gap:12px;padding:9px 12px;border-left:3px solid transparent;cursor:pointer;animation:rowin 160ms ease-out backwards}
.row:nth-child(2){animation-delay:25ms}.row:nth-child(3){animation-delay:50ms}.row:nth-child(4){animation-delay:75ms}.row:nth-child(5){animation-delay:100ms}.row:nth-child(6){animation-delay:125ms}.row:nth-child(7){animation-delay:150ms}.row:nth-child(8){animation-delay:175ms}
@keyframes rowin{from{opacity:0;transform:translateX(-4px)}to{opacity:1;transform:none}}
.row.sel{background:var(--f-surface-2);border-left-color:var(--f-accent)}
.row .idx{font-family:var(--f-font-mono);font-size:11px;color:var(--f-faint);width:20px;flex:none}
.row .tt{display:block;min-width:0;flex:1}
.row .t{display:block;font-size:14px;color:var(--f-fg);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.row .u{display:block;font-size:11px;color:var(--f-muted);font-family:var(--f-font-mono);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.row .tag{font-family:var(--f-font-mono);font-size:10px;letter-spacing:.06em;text-transform:uppercase;color:var(--f-muted);border:1px solid var(--f-border);border-radius:var(--pk-ctl-radius,6px);padding:1px 6px;flex:none}
.row .tag.on{color:var(--f-accent);border-color:var(--f-accent)}
.footer{display:flex;justify-content:space-between;padding:8px 16px;border-top:1px solid var(--f-border);font-family:var(--f-font-mono);font-size:11px;color:var(--f-muted)}
.footer b{color:var(--f-fg);font-weight:400}
.empty{padding:24px;text-align:center;font-family:var(--f-font-mono);font-size:12px;letter-spacing:.06em;color:var(--f-faint)}
@media (prefers-reduced-motion:reduce){.panel,.row{animation:none}}
</style></head>
<body>
<div class="backdrop" id="backdrop"></div>
<div class="panel" id="panel">
<div class="eyebrow"><span>Command palette</span><span id="count"></span></div>
<div class="inputrow">
<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="11" cy="11" r="7"/><line x1="16.5" y1="16.5" x2="20" y2="20"/></svg>
<input id="q" role="combobox" aria-expanded="true" aria-controls="results" aria-label="Command palette" placeholder="Type a command, setting, or tab…" autocomplete="off" spellcheck="false">
<span class="kbd">ESC</span>
</div>
<div id="results" role="listbox" aria-label="Results"></div>
<div class="footer"><span><b>&#8593;&#8595;</b> navigate &middot; <b>Enter</b> run &middot; <b>Esc</b> close</span><span>FERRITE</span></div>
</div>
<script>
(function(){
  var q=document.getElementById('q'), results=document.getElementById('results'),
      count=document.getElementById('count'), sel=0, rows=[],
      tabs=[], commands=[], settings=[], recent=[];
  function post(o){ try{ window.ipc.postMessage(JSON.stringify(o)); }catch(e){} }
  function esc(s){ return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;'); }
  // Contiguous-character fuzzy match (VS Code/fzf style): query letters in
  // order, ranked by word-boundary hits and contiguity. Synchronous over
  // the pushed snapshot, no debounce, no roundtrip, instant by design.
  function fuzzy(query, text){
    query=String(query||'').toLowerCase(); text=String(text||'').toLowerCase();
    if(!query) return 0;
    var qi=0, score=0, last=-2, run=0;
    for(var ti=0; ti<text.length && qi<query.length; ti++){
      if(text[ti]===query[qi]){
        var bonus=1;
        if(ti===0||/[\s\-_\/:.#>+]/.test(text[ti-1])) bonus+=8;
        if(ti===last+1){ run++; bonus+=6+run*2; } else run=0;
        score+=bonus; last=ti; qi++;
      }
    }
    if(qi<query.length) return -1;
    score+=Math.max(0,40-text.length);
    if(text.indexOf(query)===0) score+=25;
    return score;
  }
  function hay(r){
    if(r.kind==='tab') return r.title+' '+r.url;
    if(r.kind==='setting') return r.title+' '+r.keywords;
    return r.title+' '+r.keywords;
  }
  function rank(query){
    query=String(query||'').trim();
    var out=[];
    if(!query){
      // Empty: recent commands first (MRU), then tabs (active first),
      // then the remaining commands. Small, high-value overlap only.
      var seen={}, i;
      for(i=0;i<recent.length;i++){ var c=byId('command',recent[i]); if(c&&!seen[c.id]){ seen[c.id]=1; out.push(c); } }
      var sorted=tabs.slice().sort(function(a,b){ return (b.active?1:0)-(a.active?1:0); });
      for(i=0;i<sorted.length;i++) out.push(sorted[i]);
      for(i=0;i<commands.length;i++){ if(!seen[commands[i].id]) out.push(commands[i]); }
      return out.slice(0,12);
    }
    var scored=[], i, r, s;
    var all=tabs.concat(commands,settings);
    for(i=0;i<all.length;i++){ r=all[i]; s=fuzzy(query,hay(r)); if(s>=0) scored.push([s,r]); }
    scored.sort(function(a,b){ return b[0]-a[0]; });
    for(i=0;i<scored.length && out.length<12;i++) out.push(scored[i][1]);
    return out;
  }
  function byId(kind,id){
    var pools=kind==='command'?commands:(kind==='setting'?settings:tabs);
    for(var i=0;i<pools.length;i++) if(pools[i].id===id) return pools[i];
    return null;
  }
  window.__ferritePalette=function(data){
    data=data||{};
    tabs=data.tabs||[]; commands=data.commands||[];
    settings=data.settings||[]; recent=data.recent||[];
    render();
  };
  window.__ferriteTabs=function(list){
    tabs=list||[]; render();
  };
  window.__ferriteShow=function(){
    var p=document.getElementById('panel');
    p.style.animation='none'; void p.offsetWidth; p.style.animation='';
    q.value=''; q.focus(); post({type:'palette',action:'open'});
  };
  function tagFor(r){
    if(r.kind==='command') return '<span class="tag">COMMAND</span>';
    if(r.kind==='setting') return '<span class="tag">SETTING</span>';
    return (r.active?'<span class="tag on">ACTIVE</span>':(r.discarded?'<span class="tag">DISCARDED</span>':'<span class="tag">TAB</span>'));
  }
  function subFor(r){
    if(r.kind==='tab') return esc(r.url);
    if(r.kind==='setting') return 'Settings';
    return esc(r.hint||r.keywords||'');
  }
  function render(){
    rows=rank(q.value);
    if(sel>=rows.length) sel=0;
    count.textContent = rows.length ? (rows.length + (rows.length===1?' RESULT':' RESULTS')) : '';
    if(!rows.length){ results.innerHTML='<div class="empty">NO MATCHES</div>'; return; }
    var h='';
    for(var i=0;i<rows.length;i++){ var r=rows[i];
      h+='<div class="row'+(i===sel?' sel':'')+'" role="option" aria-selected="'+(i===sel?'true':'false')+'" data-i="'+i+'">'
        +'<span class="idx">'+('0'+(i+1)).slice(-2)+'</span>'
        +'<span class="tt"><span class="t">'+esc(r.title)+'</span><span class="u">'+subFor(r)+'</span></span>'
        +tagFor(r)
        +'</div>';
    }
    results.innerHTML=h;
    var els=results.querySelectorAll('.row'), j;
    for(j=0;j<els.length;j++){ (function(el){ el.addEventListener('click',function(){ choose(parseInt(el.getAttribute('data-i'),10)); }); })(els[j]); }
  }
  function choose(i){
    if(i>=0&&i<rows.length){ var r=rows[i]; post({type:'palette',action:'run',kind:r.kind,id:r.id}); }
  }
  q.addEventListener('input',function(){ sel=0; render(); });
  q.addEventListener('keydown',function(e){
    if(e.key==='ArrowDown'){ e.preventDefault(); if(rows.length){ sel=(sel+1)%rows.length; render(); } }
    else if(e.key==='ArrowUp'){ e.preventDefault(); if(rows.length){ sel=(sel-1+rows.length)%rows.length; render(); } }
    else if(e.key==='Enter'){ e.preventDefault(); choose(sel); }
    else if(e.key==='Escape'){ e.preventDefault(); post({type:'palette',action:'close'}); }
  });
  document.getElementById('backdrop').addEventListener('click',function(){ post({type:'palette',action:'close'}); });
  document.getElementById('panel').addEventListener('click',function(e){ e.stopPropagation(); });
  document.addEventListener('keydown',function(e){ if(e.key==='Escape'){ post({type:'palette',action:'close'}); } });
  window.__ferriteShow();
})();
</script>
</body></html>
"#;

/// Context menu overlay page: a hook-fed generic row list (the same row
/// shape as the main menu, rendered in its own overlay window because the
/// content webview sits above chrome in z-order). Rows post back
/// `ctx_activate`; Escape posts `ctx_dismiss`.
pub const CONTEXT_HTML: &str = r#"<!DOCTYPE html><!-- ferrite://ctxmenu -->
<html><head><meta charset="utf-8">
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;background:transparent;font-family:var(--f-font-ui);overflow:hidden}
#m{background:var(--f-surface-1);border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);padding:4px;animation:drop .12s ease-out;max-height:100vh;overflow-y:auto;box-sizing:border-box}
@keyframes drop{from{opacity:0;transform:translateY(-4px);}to{opacity:1;transform:none;}}
#m .mi{display:flex;align-items:center;padding:7px 10px;border-radius:var(--pk-card-radius,8px);cursor:pointer;font-size:13px;color:var(--f-fg);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
#m .mi:hover,#m .mi:focus,#m .mi.hl{background:var(--f-surface-2);outline:none}
#m .mi .k{margin-left:auto;color:var(--f-faint);font-size:12px;padding-left:12px;flex:none}
#m .mi .ck{width:14px;flex:none;color:var(--f-accent)}
#m hr{border:none;border-top:1px solid var(--f-border);margin:4px 0}
#m .v{padding:6px 10px 2px;color:var(--f-faint);font-size:11px;text-align:right}
@media (prefers-reduced-motion:reduce){#m{animation:none}}
</style></head>
<body><div id="m" role="menu" aria-label="Context menu"></div>
<script>
(function(){
  function post(o){ try{ window.ipc.postMessage(JSON.stringify(o)); }catch(e){} }
  function esc(s){ return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;'); }
  window.__ferriteCtxMenu=function(items,hl){
    menuMode=false;
    renderRows(items,hl);
    // Focus-first only for the classic menu call (no hl): the focus
    // dropdown passes hl and keeps field focus, highlighting visually.
    if(hl===undefined){ var fm=document.getElementById('m'); var first=fm.querySelector('[data-id]'); if(first) first.focus(); }
  };
  // Main application menu: same rows plus optional hint/checked/footer,
  // same overlay window (it sits above content in z-order, which chrome
  // DOM cannot). Mode switches the post types so the shell can tell menu
  // traffic from context-menu traffic on the shared channel.
  var menuMode=false;
  window.__ferriteMenu=function(items){
    menuMode=true;
    renderRows(items);
    var first=document.querySelector('#m [data-id]'); if(first) first.focus();
  };
  function renderRows(items,hl){
    var h='';
    (items||[]).forEach(function(it,i){
      if(it.sep){ h+='<hr>'; return; }
      if(it.id===undefined||it.id===null){ h+='<div class="v">'+esc(it.label||'')+'</div>'; return; }
      h+='<div class="mi'+(i===hl?' hl':'')+'" role="menuitem"'+(it.checked!==undefined?' role="menuitemcheckbox" aria-checked="'+(!!it.checked)+'"':'')+' tabindex="-1" data-id="'+esc(it.id)+'">'
        +'<span class="ck">'+(it.checked?'✓':'')+'</span>'
        +'<span>'+esc(it.label||'')+'</span>'
        +(it.hint?'<span class="k">'+esc(it.hint)+'</span>':'')
        +'</div>';
    });
    var m=document.getElementById('m'); m.innerHTML=h;
  }
  document.getElementById('m').addEventListener('click',function(e){
    var r=e.target.closest('[data-id]'); if(!r) return;
    post({type:(menuMode?'menu_activate':'ctx_activate'),id:r.getAttribute('data-id')});
  });
  document.getElementById('m').addEventListener('keydown',function(e){
    var rows=Array.prototype.slice.call(document.querySelectorAll('#m [data-id]'));
    if(!rows.length) return;
    var idx=rows.indexOf(document.activeElement);
    if(e.key==='ArrowDown'||e.key==='ArrowUp'){ e.preventDefault(); rows[e.key==='ArrowDown'?(idx+1)%rows.length:(idx-1+rows.length)%rows.length].focus(); }
    else if(e.key==='Enter'||e.key===' '){ var r=e.target.closest?e.target.closest('[data-id]'):null; if(r){ e.preventDefault(); r.click(); } }
    else if(e.key==='Escape'){ e.preventDefault(); post({type:(menuMode?'menu_dismiss':'ctx_dismiss')}); }
  });
  document.addEventListener('keydown',function(e){ if(e.key==='Escape') post({type:(menuMode?'menu_dismiss':'ctx_dismiss')}); });
})();
</script></body></html>
"#;

/// Script-dialog overlay: chrome-rendered alert/confirm/prompt/beforeunload
/// for intercepted page dialogs (Windows WebView2 `ScriptDialogOpening`;
/// engine defaults elsewhere). Never page-paintable: the page controls
/// only the message body, the origin lead, buttons, and checkbox are
/// shell chrome. The origin line comes FIRST in DOM order so screen
/// readers announce "example.com says" before the (potentially deceptive)
/// message body.
///
/// Deliberately no autofill hooks anywhere in this page: a `prompt()`
/// input is page-controlled UI, never a vault-evaluated form field, so
/// the password dropdown must never populate it (shell fills only target
/// the active content tab, see the autofill gating there).
pub const DIALOG_HTML: &str = r#"<!DOCTYPE html><!-- ferrite://dialog -->
<html><head><meta charset="utf-8">
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;background:transparent;font-family:var(--f-font-ui);overflow:hidden}
#wrap{position:fixed;inset:0;display:flex;align-items:center;justify-content:center;background:rgba(0,0,0,.45)}
#d{background:var(--f-surface-1);border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);padding:16px 18px;width:min(440px,92vw);box-sizing:border-box;animation:drop .12s ease-out}
@keyframes drop{from{opacity:0;transform:translateY(-4px);}to{opacity:1;transform:none;}}
#dorg{font-size:12px;color:var(--f-faint);margin:0 0 8px}
#dorg b{color:var(--f-fg)}
#dmsg{font-size:14px;color:var(--f-fg);margin:0 0 12px;white-space:pre-wrap;overflow-wrap:anywhere;max-height:30vh;overflow:auto}
#din{width:100%;box-sizing:border-box;background:var(--f-surface-2);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-ctl-radius,6px);padding:7px 10px;font:inherit;font-size:13px;margin:0 0 12px}
#dsup{display:flex;gap:8px;align-items:center;font-size:12px;color:var(--f-muted);margin:0 0 12px;cursor:pointer}
#drow{display:flex;gap:8px;justify-content:flex-end}
#drow button{border:1px solid var(--f-border);background:transparent;color:var(--f-fg);border-radius:var(--pk-ctl-radius,6px);padding:6px 16px;cursor:pointer;font:inherit;font-size:13px}
#drow button.primary{background:var(--f-accent);border-color:transparent;color:var(--f-bg);font-weight:700}
#drow button:hover{filter:brightness(1.15)}
@media (prefers-reduced-motion:reduce){#d{animation:none}}
</style></head>
<body><div id="wrap"><div id="d" role="alertdialog" aria-modal="true" aria-labelledby="dorg" aria-describedby="dmsg" hidden>
<div id="dorg"></div>
<p id="dmsg"></p>
<input id="din" type="text" autocomplete="off" spellcheck="false" style="display:none">
<label id="dsup" style="display:none"><input type="checkbox" id="dsupbox"> Prevent this page from creating additional dialogs</label>
<div id="drow"><button id="dcancel">Cancel</button><button id="dok" class="primary">OK</button></div>
</div></div>
<script>
(function(){
  function post(o){ try{ window.ipc.postMessage(JSON.stringify(o)); }catch(e){} }
  function esc(s){ return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;'); }
  var seq=null, kind='alert';
  window.__ferriteDialog=function(frame){
    seq=frame.seq; kind=frame.kind||'alert';
    var org=frame.origin||{};
    var lead=org.dom?esc(org.dom):esc(frame.uri||'This page');
    document.getElementById('dorg').innerHTML='<b>'+lead+'</b> says:';
    document.getElementById('dmsg').textContent=frame.message||'';
    var inp=document.getElementById('din');
    if(kind==='prompt'){ inp.style.display=''; inp.value=frame.default_text||''; }
    else inp.style.display='none';
    document.getElementById('dsup').style.display=frame.show_suppress?'':'none';
    document.getElementById('dsupbox').checked=false;
    var ok=document.getElementById('dok'), cancel=document.getElementById('dcancel');
    if(kind==='alert'){ ok.textContent='OK'; cancel.style.display='none'; }
    else if(kind==='beforeunload'){ ok.textContent='Leave'; cancel.textContent='Stay'; cancel.style.display=''; }
    else { ok.textContent='OK'; cancel.textContent='Cancel'; cancel.style.display=''; }
    document.getElementById('d').hidden=false;
    // Focus the safe control: alert has nothing to lose (OK); prompt
    // takes text entry; confirm/beforeunload land on Cancel/Stay so a
    // stray Enter never confirms.
    if(kind==='prompt'){ inp.focus(); inp.select(); }
    else if(kind==='alert'){ ok.focus(); }
    else { cancel.focus(); }
  };
  function respond(accepted){
    if(seq===null||seq===undefined) return;
    var s=seq; seq=null;
    var text=(kind==='prompt')?document.getElementById('din').value:'';
    var suppress=document.getElementById('dsupbox').checked;
    post({type:'dialog_respond',seq:s,text:text,suppress:suppress,accepted:accepted});
  }
  document.getElementById('dok').addEventListener('click',function(){ respond(true); });
  document.getElementById('dcancel').addEventListener('click',function(){ respond(false); });
  // Hard focus trap: Tab cycles only among this dialog's own controls.
  // Escape cancels (equivalent to Cancel, never Accept).
  document.addEventListener('keydown',function(e){
    if(seq===null||seq===undefined) return;
    if(e.key==='Escape'){ e.preventDefault(); respond(false); return; }
    if(e.key!=='Tab') return;
    var ctrls=Array.prototype.filter.call(
      document.querySelectorAll('#din,#dsupbox,#dok,#dcancel'),
      function(el){ return el.offsetParent!==null; });
    if(!ctrls.length) return;
    var idx=ctrls.indexOf(document.activeElement);
    e.preventDefault();
    ctrls[e.shiftKey?(idx-1+ctrls.length)%ctrls.length:(idx+1)%ctrls.length].focus();
  });
})();
</script></body></html>
"#;

/// Password manager: list/add/delete/reveal/copy over CredentialStore.
/// Usernames are metadata (plaintext rows, like history); passwords arrive
/// only via one-shot reveal or never leave Rust (copy). No lock model yet.
/// The keyring key is always available, so this page simply works; the
/// master-password wrap + re-auth gates are the tracked upgrade.
pub const PASSWORDS_HTML: &str = r#"<!DOCTYPE html><!-- ferrite://passwords -->
<html><head><meta charset="utf-8"><title>Passwords - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
#wrap{padding:24px 32px;max-width:900px}
h1{font-size:20px;margin:0 0 4px}
.sub{color:var(--f-muted);font-size:12px;margin:0 0 16px}
#q{background:var(--f-surface-1);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-card-radius,8px);padding:8px 12px;width:min(480px,90%);font-size:14px;margin-bottom:12px}
#add{background:var(--f-surface-1);border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);padding:12px;margin-bottom:16px;display:flex;gap:8px;flex-wrap:wrap;align-items:center}
#add input{background:var(--f-surface-2);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-card-radius,8px);padding:8px 10px;font-size:13px;font-family:var(--f-font-mono);flex:1;min-width:140px}
#add button{background:var(--f-accent);border:none;color:var(--f-bg);border-radius:var(--pk-ctl-radius,6px);padding:8px 14px;cursor:pointer;font-size:13px;font-weight:700;font-family:inherit}
#add button.ghost{background:none;border:1px solid var(--f-border);color:var(--f-fg);font-weight:400}
#list .r,#conflicts .r{display:flex;gap:10px;align-items:center;padding:10px 4px;border-bottom:1px solid var(--f-border);font-size:13px}
#list .av,#conflicts .av{width:24px;height:24px;border-radius:50%;background:var(--f-surface-2);color:var(--f-muted);font-size:11px;font-weight:700;display:flex;align-items:center;justify-content:center;flex:none}
#list .u,#conflicts .u{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
#list .u .url,#conflicts .u .url{display:block;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
#list .u .un,#conflicts .u .un{color:var(--f-muted);font-size:12px}
#list button,#conflicts button{background:none;border:1px solid var(--f-border);color:var(--f-muted);border-radius:var(--pk-ctl-radius,6px);padding:3px 9px;cursor:pointer;font-size:11px;flex:none;font-family:inherit}
#list button:hover,#conflicts button:hover{color:var(--f-fg)}
#list button[aria-pressed="true"]{color:var(--f-accent);border-color:var(--f-accent)}
.empty{color:var(--f-faint);font-size:13px;padding:16px 4px}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}</style></head>
<body><div id="wrap">
<h1>Passwords</h1>
<p class="sub">Stored encrypted. Unlock-free for now. A master password + lock model is planned, not present.</p>
<div role="search"><input id="q" placeholder="Filter passwords" autocomplete="off" spellcheck="false"></div>
<div id="add">
<input id="au" type="text" placeholder="https://site.example/login" aria-label="Site URL" spellcheck="false">
<input id="an" type="text" placeholder="username" aria-label="Username" spellcheck="false" autocomplete="off">
<input id="ap" type="password" placeholder="password" aria-label="Password" autocomplete="new-password">
<button id="gen" class="ghost" title="Generate">Generate</button>
<button id="save">Save</button>
<button id="exp" class="ghost" title="Export all as plaintext CSV">Export</button>
</div>
<div id="list" role="list" aria-label="Saved passwords"></div>
<div id="conflicts" aria-live="polite"></div>
</div>
<script>
(function(){
  var q=document.getElementById('q'),list=document.getElementById('list');
  var rows=[],revealed={};
  function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}
  function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');}
  function avatarOf(t){var m=String(t||'').match(/[A-Za-z0-9]/);return m?m[0].toUpperCase():'•';}
  function render(){
    var needle=q.value.trim().toLowerCase(),h='';
    rows.forEach(function(it){
      if(needle&&String(it.url).toLowerCase().indexOf(needle)<0&&String(it.username).toLowerCase().indexOf(needle)<0)return;
      var pw=revealed[it.id];
      h+='<div class="r" role="listitem" data-rid="'+it.id+'">'
        +'<span class="av">'+esc(avatarOf(it.url))+'</span>'
        +'<span class="u"><span class="url">'+esc(it.url)+'</span><span class="un">'+esc(it.username||'')+'</span></span>'
        +'<span class="pw">'+(pw?esc(pw):'••••••••')+'</span>'
        +'<button data-reveal="'+it.id+'" aria-pressed="'+(!!pw)+'" aria-label="Reveal password">Reveal</button>'
        +'<button data-copyu="'+esc(it.username||'')+'">Copy user</button>'
        +'<button data-copy="'+it.id+'">Copy pass</button>'
        +'<button data-del="'+it.id+'">Delete</button></div>';
    });
    list.innerHTML=h||'<div class="empty">No passwords saved yet.</div>';
  }
  window.__ferritePasswords=function(items){rows=items||[];revealed={};render();};
  window.__ferritePasswordReveal=function(res){
    res=res||{};
    if(res.password)revealed[res.id]=res.password;
    else delete revealed[res.id];
    render();
  };
  window.__ferritePasswordGenerated=function(res){
    if(res&&res.password)document.getElementById('ap').value=res.password;
  };
  // Import collisions: stored entry kept until the user picks per row.
  // Rows carry origin+username back so a stale index (list re-pushed on
  // every resolve) mismatches Rust-side instead of hitting the wrong row.
  window.__ferritePasswordConflicts=function(items){
    items=items||[];
    var h='';
    items.forEach(function(it){
      h+='<div class="r" role="listitem">'
        +'<span class="av">?</span>'
        +'<span class="u"><span class="url">'+esc(it.url)+'</span><span class="un">'+esc(it.username||'')+' ; imported password differs, stored kept</span></span>'
        +'<button data-keep="'+it.id+'" data-w="'+esc(it.url)+'" data-u="'+esc(it.username||'')+'">Keep mine</button>'
        +'<button data-use="'+it.id+'" data-w="'+esc(it.url)+'" data-u="'+esc(it.username||'')+'">Use imported</button></div>';
    });
    document.getElementById('conflicts').innerHTML=h
      ?'<p class="sub">Import conflicts: choose per row:</p>'+h
      :'';
  };
  document.getElementById('conflicts').addEventListener('click',function(e){
    var t=e.target;
    function pick(sel,keep){var b=t.closest?t.closest(sel):null;if(!b)return false;
      post({type:'passwords_conflict_resolve',id:+b.getAttribute(sel),url:b.getAttribute('data-w'),username:b.getAttribute('data-u'),keep:keep});return true;}
    if(pick('[data-keep]',true))return;
    if(pick('[data-use]',false))return;
  });
  list.addEventListener('click',function(e){
    var t=e.target;
    var rv=t.closest?t.closest('[data-reveal]'):null;
    if(rv){var id=rv.getAttribute('data-reveal');if(revealed[id]){delete revealed[id];render();}else post({type:'passwords_reveal',id:id});return;}
    var cu=t.closest?t.closest('[data-copyu]'):null;
    if(cu){post({type:'passwords_copy_user',text:cu.getAttribute('data-copyu')});return;}
    var cp=t.closest?t.closest('[data-copy]'):null;
    if(cp){post({type:'passwords_copy',id:cp.getAttribute('data-copy')});return;}
    var d=t.closest?t.closest('[data-del]'):null;
    if(d){post({type:'passwords_delete',id:d.getAttribute('data-del')});return;}
  });
  document.getElementById('gen').addEventListener('click',function(){post({type:'passwords_generate',len:20});});
  // Export arms in two steps: the first click names the cost (plaintext
  // CSV to disk), the second confirms. No confirmation state survives a
  // re-render (revealed map resets on every push, same as reveals).
  var expArmed=false;
  document.getElementById('exp').addEventListener('click',function(){
    if(!expArmed){ expArmed=true; this.textContent='Confirm export '+rows.length+' as plaintext CSV?'; return; }
    expArmed=false; this.textContent='Export';
    post({type:'passwords_export'});
  });
  document.getElementById('save').addEventListener('click',function(){
    var u=document.getElementById('au').value.trim();
    if(!u)return;
    post({type:'passwords_add',url:u,username:document.getElementById('an').value.trim(),password:document.getElementById('ap').value});
    document.getElementById('au').value='';document.getElementById('an').value='';document.getElementById('ap').value='';
  });
  q.addEventListener('input',render);
  q.addEventListener('keydown',function(e){if(e.key==='Escape')post({type:'dismiss'});});
  post({type:'passwords_query'});
})();
</script></body></html>
"#;

/// Autofill manager: address profiles + payment cards over AutofillStore.
/// Cards show network + last4 only (the full PAN is decrypted Rust-side on
/// an id-based copy post and written via the sensitive clipboard path:
/// private formats plus auto-clear; never into this page). No CVV input
/// exists anywhere: unknown form keys are dropped by the struct shape
/// Rust-side.
/// Query contract: `?section=addresses|payment-methods`, `?highlight=id`.
pub const AUTOFILL_HTML: &str = r#"<!DOCTYPE html><!-- ferrite://autofill -->
<html><head><meta charset="utf-8"><title>Autofill - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
#wrap{padding:24px 32px;max-width:900px}
h1{font-size:20px;margin:0 0 4px}h2{font-size:13px;letter-spacing:.06em;text-transform:uppercase;color:var(--f-muted);font-weight:400;margin:28px 0 8px}
.sub{color:var(--f-muted);font-size:12px;margin:0 0 8px}
.frm{background:var(--f-surface-1);border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);padding:12px;margin-bottom:8px;display:grid;grid-template-columns:1fr 1fr 1fr;gap:8px}
.frm input{background:var(--f-surface-2);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-card-radius,8px);padding:8px 10px;font-size:13px;font-family:inherit;min-width:0}
.frm button{grid-column:1/-1;background:var(--f-accent);border:none;color:var(--f-bg);border-radius:var(--pk-ctl-radius,6px);padding:8px 14px;cursor:pointer;font-size:13px;font-weight:700;font-family:inherit;justify-self:start}
.err{color:var(--f-danger);font-size:12px;min-height:18px;margin-bottom:4px}
.r{display:flex;gap:10px;align-items:center;padding:10px 4px;border-bottom:1px solid var(--f-border);font-size:13px}
.r .av{width:24px;height:24px;border-radius:50%;background:var(--f-surface-2);color:var(--f-muted);font-size:11px;font-weight:700;display:flex;align-items:center;justify-content:center;flex:none}
.r .t{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.r .s{color:var(--f-faint);font-size:11px;flex:none;font-variant-numeric:tabular-nums}
.r button{background:none;border:1px solid var(--f-border);color:var(--f-muted);border-radius:var(--pk-ctl-radius,6px);padding:3px 9px;cursor:pointer;font-size:11px;flex:none;font-family:inherit}
.r button:hover{color:var(--f-fg)}
.r.flash{outline:2px solid var(--f-accent);outline-offset:-2px}
.empty{color:var(--f-faint);font-size:13px;padding:12px 4px}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}</style></head>
<body><div id="wrap">
<h1>Autofill</h1>
<p class="sub">Addresses fill freely; cards fill full numbers into the field only. CVV is never stored or filled. Always typed by hand.</p>
<h2 id="sec-addresses">Addresses</h2>
<div class="frm" id="afrm">
<input data-k="label" placeholder="Label (Home)" aria-label="Address label">
<input data-k="name" placeholder="Full name" aria-label="Full name">
<input data-k="street" placeholder="Street" aria-label="Street">
<input data-k="city" placeholder="City" aria-label="City">
<input data-k="region" placeholder="Region" aria-label="Region">
<input data-k="postal" placeholder="Postal code" aria-label="Postal code">
<input data-k="country" placeholder="Country" aria-label="Country">
<input data-k="phone" placeholder="Phone" aria-label="Phone">
<input data-k="email" placeholder="Email" aria-label="Email">
<button id="asave">Save address</button>
</div>
<div class="err" id="aerr" aria-live="polite"></div>
<div id="alist" role="list" aria-label="Saved addresses"></div>
<h2 id="sec-cards">Payment methods</h2>
<div class="frm" id="cfrm">
<input data-k="label" placeholder="Label (Personal)" aria-label="Card label">
<input data-k="name" placeholder="Name on card" aria-label="Name on card">
<input data-k="pan" placeholder="Card number" inputmode="numeric" aria-label="Card number" autocomplete="off">
<input data-k="expiry" placeholder="MM/YY" aria-label="Expiry" autocomplete="off">
<button id="csave">Save card</button>
</div>
<div class="err" id="cerr" aria-live="polite"></div>
<div id="clist" role="list" aria-label="Saved cards"></div>
</div>
<script>
(function(){
  var alist=document.getElementById('alist'),clist=document.getElementById('clist');
  var addrs=[],cards=[],flashId='';
  function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}
  function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');}
  function avatarOf(t){var m=String(t||'').match(/[A-Za-z0-9]/);return m?m[0].toUpperCase():'•';}
  function addrLine(f){
    f=f||{};
    return [f.name,f.street,f.city,f.region,f.postal,f.country].filter(function(x){return x;}).join(', ');
  }
  function render(){
    var h='';
    addrs.forEach(function(it){
      h+='<div class="r'+(flashId&&it.id===flashId?' flash':'')+'" role="listitem" data-rid="'+it.id+'">'
        +'<span class="av">'+esc(avatarOf(it.label))+'</span>'
        +'<span class="t">'+esc(it.label)+', '+esc(addrLine(it.fields))+'</span>'
        +'<button data-adel="'+it.id+'">Delete</button></div>';
    });
    alist.innerHTML=h||'<div class="empty">No addresses saved.</div>';
    h='';
    cards.forEach(function(it){
      h+='<div class="r'+(flashId&&it.id===flashId?' flash':'')+'" role="listitem" data-rid="'+it.id+'">'
        +'<span class="av">'+esc(avatarOf(it.network))+'</span>'
        +'<span class="t">'+esc(it.label)+', '+esc(it.network)+' •••• •••• •••• '+esc(it.last4)+'</span>'
        +'<span class="s">'+esc(it.expiry||'')+'</span>'
        +'<button data-ccopy="'+it.id+'">Copy number</button>'
        +'<button data-cdel="'+it.id+'">Delete</button></div>';
    });
    clist.innerHTML=h||'<div class="empty">No cards saved.</div>';
    if(flashId){
      var f=document.querySelector('[data-rid="'+flashId+'"]');
      flashId='';
      if(f){try{f.scrollIntoView({block:'center'});}catch(_){}setTimeout(function(){f.classList.remove('flash');},2500);}
    }
  }
  window.__ferriteAutofill=function(d){
    d=d||{};addrs=d.addresses||[];cards=d.cards||[];
    document.getElementById('aerr').textContent='';
    document.getElementById('cerr').textContent='';
    render();
  };
  window.__ferriteAutofillError=function(res){
    res=res||{};
    document.getElementById('cerr').textContent=res.message||'Could not save.';
  };
  function collect(form){
    var o={};
    form.querySelectorAll('[data-k]').forEach(function(i){o[i.getAttribute('data-k')]=i.value;});
    return o;
  }
  function clearForm(form){form.querySelectorAll('[data-k]').forEach(function(i){i.value='';});}
  document.getElementById('asave').addEventListener('click',function(){
    post({type:'autofill_add_address',fields:collect(document.getElementById('afrm'))});
    clearForm(document.getElementById('afrm'));
  });
  document.getElementById('csave').addEventListener('click',function(){
    post({type:'autofill_add_card',fields:collect(document.getElementById('cfrm'))});
    clearForm(document.getElementById('cfrm'));
  });
  document.getElementById('wrap').addEventListener('click',function(e){
    var cc=e.target.closest?e.target.closest('[data-ccopy]'):null;
    if(cc){post({type:'autofill_copy_card',id:cc.getAttribute('data-ccopy')});return;}
    var a=e.target.closest?e.target.closest('[data-adel]'):null;
    if(a){post({type:'autofill_delete',id:a.getAttribute('data-adel')});return;}
    var c=e.target.closest?e.target.closest('[data-cdel]'):null;
    if(c){post({type:'autofill_delete',id:c.getAttribute('data-cdel')});return;}
  });
  try{
    var sp=new URLSearchParams(location.search);
    flashId=sp.get('highlight')||'';
    var sec=sp.get('section')||'';
    var target=sec==='payment-methods'?'sec-cards':(sec==='addresses'?'sec-addresses':'');
    if(target){var el=document.getElementById(target);if(el)el.scrollIntoView();}
  }catch(_){}
  post({type:'autofill_query'});
})();
</script></body></html>
"#;

/// About page: static document (version, engine, page index, legal).
/// Runtime values are baked at serve time, no IPC, no hooks, nothing
/// live. Deliberately the plainest page: headings, a copy button, anchors.
/// About masthead: the Ferrite banner itself (theme-aware light/dark
/// pair, same prefers-color-scheme mechanism as the welcome mark).
/// Inlined: offline-safe, zero fetches (`img-src 'none'` forbids remote
/// art on privileged pages by construction).
pub const ABOUT_HTML: &str = concat!(
    r##"<!DOCTYPE html><!-- ferrite://about -->
<html><head><meta charset="utf-8"><title>About - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
#wrap{padding:24px 32px;max-width:720px}
h1{font-size:22px;margin:0 0 4px}h2{font-size:14px;margin:28px 0 8px;letter-spacing:.04em}
.sub{color:var(--f-muted);font-size:13px;margin:0 0 16px}
nav.toc{display:flex;gap:16px;margin:0 0 8px;font-size:13px}
a{color:var(--f-accent)}
table{border-collapse:collapse;font-size:13px;margin:8px 0 0}
td{padding:5px 16px 5px 0;vertical-align:top}
td.k{color:var(--f-muted);white-space:nowrap}
ul.pages{list-style:none;margin:8px 0 0;padding:0;font-size:13px}
ul.pages li{padding:5px 0;border-bottom:1px solid var(--f-border)}
ul.pages .d{color:var(--f-muted);font-size:12px}
button{background:none;border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-ctl-radius,6px);padding:7px 14px;cursor:pointer;font-size:13px;font-family:inherit;margin-top:12px}
button:hover{border-color:var(--f-accent)}
.small{color:var(--f-muted);font-size:12px}
.brandbanner{margin:0 0 20px}
.brandbanner svg{width:100%;height:auto;display:block;border-radius:var(--pk-card-radius,8px)}
.brandbanner .mark-light{display:none}
@media (prefers-color-scheme:light){.brandbanner .mark-light{display:block}.brandbanner .mark-dark{display:none}}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}</style></head>
<body><div id="wrap">
<div class="brandbanner" aria-hidden="true"><span class="mark-dark">"##,
    include_str!("../../../brand/dark_banner.svg"),
    r##"</span><span class="mark-light">"##,
    include_str!("../../../brand/light_banner.svg"),
    r##"</span></div>
<h1>About Ferrite</h1>
<p class="sub">A privacy-first desktop browser. No telemetry exists in this product. There is nothing to opt out of. <a href="ferrite://welcome">Replay the welcome tour</a>.</p>
<div id="updated" hidden style="border:1px solid var(--f-accent);border-radius:var(--pk-card-radius,8px);padding:12px 14px;font-size:13px;margin:0 0 16px"></div>
<nav class="toc" aria-label="About sections"><a href="#version">Version</a><a href="#storage">What is encrypted</a><a href="#pages">Internal pages</a><a href="#legal">Legal</a></nav>
<section id="version"><h2>Version</h2>
<table>
<tr><td class="k">Ferrite</td><td>/*__FERRITE_ABOUT_VERSION__*/</td></tr>
<tr><td class="k">Web engine</td><td>/*__FERRITE_ABOUT_ENGINE__*/<div class="small">Engine patches arrive with the OS or the WebView2 Runtime, outside Ferrite's updates, <a href="https://go.microsoft.com/fwlink/p/?LinkId=2124703">install or update the runtime</a> (Windows) or run system updates (macOS/Linux).</div></td></tr>
<tr><td class="k">Platform</td><td>/*__FERRITE_ABOUT_OS__*/</td></tr>
</table>
<div class="small">Crash reports stay on this device (<code>ferrite-crash.log</code>, never sent anywhere). Note the boundary: the WebView2 engine may send its own crash diagnostics to Microsoft under Windows Settings → Privacy &amp; Security → Diagnostics &amp; feedback, which Ferrite cannot disable. We turn off everything we control and point here for the rest.</div>
<button id="copy">Copy diagnostics</button>
</section>
<section id="updates"><h2>Updates</h2>
<p class="sub" id="updateStatus" role="status">Checking for updates runs at launch and hourly in the background. Checks send nothing: the manifest is fetched, never posted to.</p>
<p><button id="updateCheck">Check for updates now</button> <button id="updateRestart" hidden>Restart now</button></p>
</section>
<section id="storage"><h2>What is encrypted</h2>
<p class="sub">History, bookmarks, passwords, and autofill are encrypted at rest. Cached page content (images, scripts, HTML) lives in the web engine's own cache in <code>ferrite-profile</code>, which is not per-app encrypted, its protection is your OS's full-disk encryption. Private tabs (Ctrl+Shift+N) never touch that cache: they run in the engine's non-persistent store and vanish with the tab.</p></section>
<section id="pages"><h2>Internal pages</h2>
<p class="sub">Everything Ferrite can show without the network, auditable in one place.</p>
<ul class="pages">
<li><a href="ferrite://newtab">New Tab</a><div class="d">Start page: search, top sites, blocked counts, reading list.</div></li>
<li><a href="ferrite://history">History</a><div class="d">Visits with search, filters, and per-site delete.</div></li>
<li><a href="ferrite://downloads">Downloads</a><div class="d">Completed and in-progress files with verification.</div></li>
<li><a href="ferrite://bookmarks">Bookmarks</a><div class="d">Two-pane manager: tree, table, duplicates.</div></li>
<li><a href="ferrite://passwords">Passwords</a><div class="d">Encrypted credentials: list, add, reveal, copy.</div></li>
<li><a href="ferrite://autofill">Autofill</a><div class="d">Addresses and payment methods for form fill.</div></li>
<li><a href="ferrite://settings">Settings</a><div class="d">Preferences, privacy controls, exclusions, import/export.</div></li>
<li><a href="ferrite://about">About</a><div class="d">This page.</div></li>
</ul>
</section>
<section id="legal"><h2>Legal</h2>
<p class="sub">Open-source attributions: Phosphor Icons (MIT) · Koboyo Icons (free commercial, no attribution required) · wry / winit / tokio / rusqlite (MIT / Apache-2.0) · Hunspell dictionaries (LGPL/GPL/MPL, bundled for offline spellchecking). Full dependency licenses ship in the repository and on the <a href="ferrite://credits">credits page</a>. Ferrite itself is MIT OR Apache-2.0.</p></section>
</div>
<script>
(function(){
  function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}
  document.getElementById('copy').addEventListener('click',function(){
    post({type:'about_copy'});
  });
  function paintUpdate(s){
    s=s||{};
    var el=document.getElementById('updateStatus');
    if(!el)return;
    var t={
      idle:'Checking for updates runs at launch and hourly in the background.',
      checking:'Checking for updates…',
      downloading:'Downloading update…',
      ready:'An update is downloaded. Restart Ferrite to apply it.',
      failed:'The last update check failed.',
      uptodate:'Ferrite is up to date.'
    }[s.phase||'idle']||'Checking for updates runs at launch and hourly in the background.';
    if(s.phase==='downloading'&&typeof s.downloaded==='number'){
      t+=' '+s.downloaded+' bytes'+(typeof s.total==='number'?' of '+s.total:'')+'.';
    }
    if(s.phase==='ready'&&s.version){ t+=' Version '+s.version+'.'; }
    if(s.phase==='failed'&&s.message){ t+=' '+s.message; }
    el.textContent=t;
    try{
      document.getElementById('updateRestart').hidden=(s.phase!=='ready');
    }catch(e){}
  }
  window.__ferriteUpdate=function(s){ try{ paintUpdate(s); }catch(e){} };
  document.getElementById('updateCheck').addEventListener('click',function(){
    paintUpdate({phase:'checking'});
    post({type:'update_check'});
  });
  document.getElementById('updateRestart').addEventListener('click',function(){
    post({type:'update_restart'});
  });
  // Post-update banner (?updated=<prev version>): shown once, driven by
  // the query the shell opens after an applied update. The value is
  // text-only (validated charset server-side) and always escaped.
  try{
    var q=new URLSearchParams(location.search).get('updated')||'';
    if(/^[0-9A-Za-z.\-+]{1,32}$/.test(q)){
      var b=document.getElementById('updated');
      var s=document.createElement('span');
      s.textContent='Ferrite just updated from version '+q+'. Your tabs, settings, and vault carried over untouched; the version table below confirms the running build.';
      b.textContent=''; b.appendChild(s); b.hidden=false;
    }
  }catch(e){}
  post({type:'update_query'});
})();
</script></body></html>
"##
);

/// Generated attribution snapshot (cargo-about JSON, committed, see
/// `about.toml`; regenerate with `cargo about generate --format json -o
/// crates/ferrite-tabs/assets/credits.json`). Parsed once per process.
/// After regenerating, scrub machine-absolute `source_path`/`path` values
/// to crate-relative tails before committing: the snapshot ships in the
/// binary, and CI fails loud if maintainer paths leak back in.
const CREDITS_JSON: &str = include_str!("../assets/credits.json");

struct CreditGroup {
    id: String,
    name: String,
    text: String,
    crates: Vec<(String, String)>,
}

static CREDIT_GROUPS: std::sync::OnceLock<Vec<CreditGroup>> = std::sync::OnceLock::new();

/// License groups: first-of-kind texts, crates unioned per license id
/// (cargo-about repeats ids per license file, display dedupes).
fn credit_groups() -> &'static [CreditGroup] {
    CREDIT_GROUPS.get_or_init(|| {
        let mut order: Vec<String> = Vec::new();
        let mut texts: std::collections::HashMap<String, (String, String)> =
            std::collections::HashMap::new();
        let mut members: std::collections::HashMap<
            String,
            std::collections::BTreeSet<(String, String)>,
        > = std::collections::HashMap::new();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(CREDITS_JSON)
            && let Some(list) = v.get("licenses").and_then(|x| x.as_array())
        {
            for lic in list {
                let id = lic
                    .get("id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_owned();
                if id.is_empty() {
                    continue;
                }
                if !texts.contains_key(&id) {
                    order.push(id.clone());
                }
                let first = lic
                    .get("first_of_kind")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                if first || !texts.contains_key(&id) {
                    texts.insert(
                        id.clone(),
                        (
                            lic.get("name")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_owned(),
                            lic.get("text")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_owned(),
                        ),
                    );
                }
                let set = members.entry(id.clone()).or_default();
                if let Some(used) = lic.get("used_by").and_then(|x| x.as_array()) {
                    for u in used {
                        if let Some(c) = u.get("crate") {
                            let name = c
                                .get("name")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_owned();
                            let version = c
                                .get("version")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_owned();
                            if !name.is_empty() {
                                set.insert((name, version));
                            }
                        }
                    }
                }
            }
        }
        order
            .into_iter()
            .filter_map(|id| {
                let (name, text) = texts.remove(&id)?;
                let crates = members
                    .remove(&id)
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
                Some(CreditGroup {
                    id,
                    name,
                    text,
                    crates,
                })
            })
            .collect()
    })
}

/// Curated non-Cargo entries (bundled assets + platform engines, things
/// cargo-about can never surface). Reviewed copy, not generated.
const MANUAL_CREDITS: &str = r#"<section class="lic" data-lic="manual"><h2>Bundled assets &amp; platform components</h2><p class="note">Curated entries, not generated from Cargo. Verified by hand.</p><ul>
<li data-c="phosphor icons mit"><span class="cn">Phosphor Icons</span> <span class="cv">MIT, © 2020-2024 Phosphor Icons</span><div class="cd">Icon kit bundled with the chrome (Regular/Bold/Fill weights).</div></li>
<li data-c="koboyo icons illustration commercial"><span class="cn">Koboyo Icons</span> <span class="cv">Free for commercial use, no attribution required (koboyo.com/icons/license)</span><div class="cd">Hand-drawn "Original"-style illustrations vendored as static SVG for empty/error states (offline-safe, no fetches). Not redistributed as a library.</div></li>
<li data-c="geist pixel ofl"><span class="cn">Geist Pixel</span> <span class="cv">SIL OFL 1.1, © 2024 Vercel (The Geist Project Authors)</span><div class="cd">Pixel display face for chrome accents.</div></li>
<li data-c="jetbrains mono ofl"><span class="cn">JetBrains Mono</span> <span class="cv">SIL OFL 1.1, © JetBrains</span><div class="cd">Monospace face for technical readouts.</div></li>
<li data-c="sqlite public domain"><span class="cn">SQLite</span> <span class="cv">Public domain, bundled via rusqlite (the sqlcipher feature is off, so no dual-license terms apply)</span><div class="cd">Local vault storage engine.</div></li>
<li data-c="webview2 windows"><span class="cn">WebView2 Runtime (Windows)</span> <span class="cv">Microsoft redistribution terms apply</span><div class="cd">OS web-rendering component on Windows; not a Rust crate.</div></li>
<li data-c="webkit linux macos"><span class="cn">WebKitGTK (Linux) / WKWebView (macOS)</span> <span class="cv">LGPL / BSD mix per platform component</span><div class="cd">OS web-rendering components; see platform documentation for exact terms.</div></li>
<li data-c="readability mozilla apache"><span class="cn">Readability.js 0.6.0</span> <span class="cv">Apache-2.0, © Mozilla, Arc90 Inc</span><div class="cd">Article extraction for reader mode (vendored, evaluated on demand).</div></li>
<li data-c="dompurify apache mpl"><span class="cn">DOMPurify 3.4.15</span> <span class="cv">Apache-2.0 AND MPL-2.0, © Cure53 and contributors</span><div class="cd">Output sanitizer for reader mode (vendored, evaluated on demand).</div></li>
<li data-c="hunspell dictionaries lgpl gpl mpl"><span class="cn">Hunspell dictionaries</span> <span class="cv">LGPL-2.1 OR GPL-2.0 OR MPL-1.1, © Kevin Hendricks and contributors</span><div class="cd">Bundled .bdic word lists so inline spellchecking never fetches from a CDN (see crates/ferrite-tabs/assets/dicts/README.md).</div></li>
</ul></section>"#;

/// Credits page: generated groups (license text once per group, packages
/// listed beneath) + curated manual section. Mono for names/versions/ids,
/// body font for license prose. Filter matches DOM text (no data needed
/// client-side).
pub const CREDITS_HTML: &str = r#"<!DOCTYPE html><!-- ferrite://credits -->
<html><head><meta charset="utf-8"><title>Credits - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
#wrap{padding:24px 32px;max-width:900px}
h1{font-size:20px;margin:0 0 4px}.sub{color:var(--f-muted);font-size:13px;margin:0 0 16px}
#q{background:var(--f-surface-1);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-card-radius,8px);padding:8px 12px;width:min(480px,90%);font-size:14px;margin-bottom:16px}
section.lic{margin-bottom:28px}
section.lic h2{font-size:14px;margin:0 0 4px;font-family:var(--f-font-mono);font-weight:400}
section.lic h2 .n{color:var(--f-muted);font-size:12px}
section.lic pre{white-space:pre-wrap;font-size:11px;color:var(--f-muted);background:var(--f-surface-1);border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);padding:12px;max-height:220px;overflow-y:auto;font-family:var(--f-font-ui)}
section.lic ul{list-style:none;margin:8px 0 0;padding:0}
section.lic li{padding:3px 0;font-size:12px;border-bottom:1px solid var(--f-border)}
.cn{font-family:var(--f-font-mono);color:var(--f-fg)}
.cv{font-family:var(--f-font-mono);color:var(--f-muted)}
.cd{color:var(--f-faint);font-size:11px}
.note{font-size:12px;color:var(--f-faint)}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}</style></head>
<body><div id="wrap">
<h1>Open-source credits</h1>
<p class="sub">Every dependency Ferrite ships, grouped by license. Generated from Cargo.lock, never hand-maintained.</p>
<div role="search"><input id="q" placeholder="Filter packages or licenses" autocomplete="off" spellcheck="false" aria-label="Filter credits"></div>
<div id="groups">/*__FERRITE_CREDITS__*/</div>
</div>
<script>
(function(){
  var q=document.getElementById('q');
  q.addEventListener('input',function(){
    var needle=q.value.trim().toLowerCase();
    document.querySelectorAll('#groups section.lic').forEach(function(sec){
      var any=false;
      sec.querySelectorAll('li').forEach(function(li){
        var hit=!needle||li.textContent.toLowerCase().indexOf(needle)>=0;
        li.style.display=hit?'':'none';
        if(hit)any=true;
      });
      var head=!needle||sec.querySelector('h2').textContent.toLowerCase().indexOf(needle)>=0;
      sec.style.display=(any||head||!needle)?'':'none';
    });
  });
})();
</script></body></html>
"#;

/// Render generated groups into the page shell (all text escaped, crate
/// metadata is third-party content).
fn credits_page() -> String {
    let mut h = String::new();
    for g in credit_groups() {
        h.push_str(&format!(
            "<section class=\"lic\" data-lic=\"{}\"><h2>{}, {} <span class=\"n\">({} crates)</span></h2><pre>{}</pre><ul>",
            html_escape(&g.id.to_lowercase()),
            html_escape(&g.id),
            html_escape(&g.name),
            g.crates.len(),
            html_escape(&g.text),
        ));
        for (name, version) in &g.crates {
            h.push_str(&format!(
                "<li data-c=\"{} {}\"><span class=\"cn\">{}</span> <span class=\"cv\">{}</span></li>",
                html_escape(&name.to_lowercase()),
                html_escape(version),
                html_escape(name),
                html_escape(version),
            ));
        }
        h.push_str("</ul></section>");
    }
    h.push_str(MANUAL_CREDITS);
    CREDITS_HTML.replacen("/*__FERRITE_CREDITS__*/", &h, 1)
}

/// Task manager: process table (sysinfo children, no tab↔PID mapping
/// exists) + tab inventory with End task. Keyed rows, client sort, polite
/// announcements only for vanished processes, ticking numbers never
/// touch the live region.
pub const TASKMANAGER_HTML: &str = r#"<!DOCTYPE html><!-- ferrite://taskmanager -->
<html><head><meta charset="utf-8"><title>Task Manager - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
#wrap{padding:24px 32px;max-width:1000px}
h1{font-size:20px;margin:0 0 4px}.sub{color:var(--f-muted);font-size:12px;margin:0 0 16px}
h2{font-size:13px;letter-spacing:.06em;text-transform:uppercase;color:var(--f-muted);font-weight:400;margin:24px 0 8px}
table{border-collapse:collapse;width:100%;font-size:12px}
th{font-family:var(--f-font-mono);font-size:11px;color:var(--f-muted);text-align:left;padding:6px 10px;border-bottom:1px solid var(--f-border);white-space:nowrap;font-weight:400}
th[data-sort]{cursor:pointer}
th[data-sort]:hover{color:var(--f-fg)}
td{padding:6px 10px;border-bottom:1px solid var(--f-border);overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:340px}
td.num{font-family:var(--f-font-mono);font-variant-numeric:tabular-nums;text-align:right;white-space:nowrap}
.av{width:18px;height:18px;border-radius:50%;background:var(--f-surface-2);color:var(--f-muted);font-size:9px;font-weight:700;display:inline-flex;align-items:center;justify-content:center;flex:none}
button{background:none;border:1px solid var(--f-border);color:var(--f-muted);border-radius:var(--pk-ctl-radius,6px);padding:2px 8px;cursor:pointer;font-size:11px;font-family:inherit;flex:none}
button:hover{color:var(--f-fg)}
button.danger:hover{color:var(--f-danger);border-color:var(--f-danger)}
.badge{font-size:10px;letter-spacing:.06em;text-transform:uppercase;color:var(--f-muted);border:1px solid var(--f-border);border-radius:var(--pk-ctl-radius,6px);padding:1px 6px;flex:none}
.badge.bad{color:var(--f-danger);border-color:var(--f-danger)}
#audit{font-size:12px;color:var(--f-muted);margin:4px 0 0}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}</style></head>
<body><div id="wrap">
<h1>Task Manager</h1>
<p class="sub">Live processes plus every open tab. PIDs cannot be mapped to tabs, no engine API exposes it, so the renderer-vs-tab count below is the coarse isolation audit.</p>
<p id="audit" aria-live="off"></p>
<h2>Processes</h2>
<table aria-label="Processes"><thead><tr>
<th data-sort="pid">PID</th><th data-sort="kind">Kind</th><th data-sort="name">Name</th><th data-sort="cpu">CPU %</th><th data-sort="mem">Memory</th><th></th>
</tr></thead><tbody id="procs"></tbody></table>
<h2>Tabs</h2>
<table aria-label="Tabs"><thead><tr>
<th>Tab</th><th>State</th><th title="Times unloaded this session">Sleeps</th><th>Idle</th><th>Auto-sleep</th><th></th>
</tr></thead><tbody id="tabs"></tbody></table>
<div id="sr-live" aria-live="polite" style="position:absolute;left:-9999px;"></div>
</div>
<script>
(function(){
  var procsEl=document.getElementById('procs'),tabsEl=document.getElementById('tabs'),
      audit=document.getElementById('audit'),live=document.getElementById('sr-live');
  var procs=[],tabs=[],sortKey='mem',sortDir=-1,knownPids={},totals=null;
  function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}
  function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');}
  function fmtMem(b){if(b==null)return'';if(b<1048576)return Math.max(1,Math.round(b/1024))+' KB';if(b<1073741824)return (b/1048576).toFixed(1)+' MB';return (b/1073741824).toFixed(2)+' GB';}
  function avatarOf(t){var m=String(t||'').match(/[A-Za-z0-9]/);return m?m[0].toUpperCase():'•';}
  // Keyed rows (by pid / tab id): updates move nodes, never remount.
  var procNodes={},tabNodes={};
  function procCells(it){
    return '<td class="num">'+it.pid+'</td><td>'+esc(it.kind)+'</td><td>'+esc(it.name)+'</td>'
      +'<td class="num">'+it.cpu.toFixed(1)+'</td><td class="num">'+fmtMem(it.mem)+'</td>'
      +'<td>'+(it.kind==='browser'?'':'<button data-kill="'+it.pid+'" class="danger" title="End this process">End process</button>')+'</td>';
  }
  function tabCells(it){
    var badge=it.state==='crashed'?'<span class="badge bad">crashed</span>'
      :it.state==='inactive'?'<span class="badge">inactive</span>'
      :it.state==='loading'?'<span class="badge">loading</span>':'';
    var idle=(typeof it.idle_secs==='number')?it.idle_secs+'s idle':'';
    var auto=it.auto_sleep===false?'<span class="badge" title="'+esc(it.sleep_why||'exempt')+'">no-auto</span>':'';
    return '<td><span style="display:flex;gap:8px;align-items:center;"><span class="av">'+esc(avatarOf(it.title||it.url))+'</span>'
      +'<span style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap;">'+esc(it.title||it.url||'New Tab')+'</span></span></td>'
      +'<td>'+badge+'</td>'
      +'<td class="num" title="Times unloaded this session">'+(it.discards||0)+'</td>'
      +'<td>'+esc(idle)+'</td>'
      +'<td>'+auto+'</td>'
      +'<td style="white-space:nowrap;">'+(it.state==='inactive'?'':'<button data-end="'+it.id+'" title="Free this tab\u2019s renderer">End task</button>')
      +' <button data-close="'+it.id+'" title="Close tab">Close</button></td>';
  }
  function render(){
    var seen={},seenTabs={},gone=[];
    var sorted=procs.slice().sort(function(a,b){
      var x=a[sortKey],y=b[sortKey];
      if(typeof x==='string')return sortDir*x.localeCompare(y);
      return sortDir*(x-y);
    });
    procsEl.textContent='';
    sorted.forEach(function(it){
      var key='p:'+it.pid;seen[key]=1;
      var r=procNodes[key];
      if(!r){r=document.createElement('tr');procNodes[key]=r;}
      r.innerHTML=procCells(it);
      procsEl.appendChild(r);
    });
    tabsEl.textContent='';
    tabs.forEach(function(it){
      var key='t:'+it.id;seenTabs[key]=1;
      var r=tabNodes[key];
      if(!r){r=document.createElement('tr');tabNodes[key]=r;}
      r.innerHTML=tabCells(it);
      tabsEl.appendChild(r);
    });
    Object.keys(knownPids).forEach(function(pid){
      if(!seen[pid]){gone.push(pid);delete procNodes[pid];delete knownPids[pid];}
    });
    Object.keys(tabNodes).forEach(function(k){if(!seenTabs[k]){tabNodes[k].remove();delete tabNodes[k];}});
    if(gone.length)live.textContent='Process'+(gone.length>1?'es':'')+' ended: '+gone.map(function(p){return p.slice(2);}).join(', ');
    if(totals){
      audit.textContent=totals.renderers+' renderer process'+(totals.renderers===1?'':'es')+' for '+totals.tab_count+' tabs'
        +', '+fmtMem(totals.mem_total)+', '+totals.cpu_total.toFixed(1)+'% CPU across Ferrite processes.'
        +(totals.engine_profile_bytes?(' Engine profile '+fmtMem(totals.engine_profile_bytes)+' (cache, cookies, storage: engine-owned; clear via Settings, Clear browsing data).'):'')
        +(totals.filter_lists&&totals.filter_lists.length?(' Filter lists (source text size, not heap): '+totals.filter_lists.map(function(l){return l.name+' : '+l.rules+' rules, '+fmtMem(l.bytes);}).join('; ')+'.'):'');
    }
    Object.keys(procNodes).forEach(function(k){knownPids[k]=1;});
  }
  window.__ferriteTaskman=function(d){
    d=d||{};procs=d.processes||[];tabs=d.tabs||[];
    totals={renderers:d.renderers||0,tab_count:d.tab_count||0,mem_total:d.mem_total||0,cpu_total:d.cpu_total||0,engine_profile_bytes:d.engine_profile_bytes||0,filter_lists:d.filter_lists||[]};
    render();
  };
  document.querySelectorAll('th[data-sort]').forEach(function(th){
    th.addEventListener('click',function(){
      var k=th.getAttribute('data-sort');
      if(sortKey===k)sortDir=-sortDir;else{sortKey=k;sortDir=1;}
      render();
    });
  });
  document.getElementById('wrap').addEventListener('click',function(e){
    var k=e.target.closest?e.target.closest('[data-kill]'):null;
    if(k){post({type:'taskman_kill',pid:parseInt(k.getAttribute('data-kill'),10)||0});return;}
    var t=e.target.closest?e.target.closest('[data-end]'):null;
    if(t){post({type:'taskman_end_task',id:t.getAttribute('data-end')});return;}
    var c=e.target.closest?e.target.closest('[data-close]'):null;
    if(c){post({type:'taskman_close',id:c.getAttribute('data-close')});return;}
  });
  post({type:'taskman_query'});
})();
</script></body></html>
"#;

/// Certificate viewer: summary + expandable chain for one host, fetched
/// on explicit request (a second connection, not the tab's own, stated
/// on the page). Chain as native disclosure elements; fingerprints and
/// serials selectable with copy buttons (reused file-copy post).
pub const CERTS_HTML: &str = r#"<!DOCTYPE html><!-- ferrite://certs -->
<html><head><meta charset="utf-8"><title>Certificate - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
#wrap{padding:24px 32px;max-width:800px}
h1{font-size:20px;margin:0 0 4px}h2{font-size:13px;letter-spacing:.06em;text-transform:uppercase;color:var(--f-muted);font-weight:400;margin:24px 0 8px}
.sub{color:var(--f-muted);font-size:12px;margin:0 0 8px}
.badge{font-size:11px;letter-spacing:.06em;text-transform:uppercase;color:var(--f-accent);border:1px solid var(--f-accent);border-radius:var(--pk-ctl-radius,6px);padding:2px 8px}
.badge.bad{color:var(--f-danger);border-color:var(--f-danger)}
.f{display:flex;gap:12px;padding:6px 0;border-bottom:1px solid var(--f-border);font-size:13px;align-items:baseline}
.f .k{color:var(--f-muted);width:130px;flex:none}
.f .v{flex:1;min-width:0;overflow-wrap:anywhere;user-select:text}
.f button{background:none;border:1px solid var(--f-border);color:var(--f-muted);border-radius:var(--pk-ctl-radius,6px);padding:2px 8px;cursor:pointer;font-size:11px;flex:none;font-family:inherit}
.f button:hover{color:var(--f-fg)}
details{border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);margin-bottom:8px;padding:4px 12px}
summary{cursor:pointer;font-size:13px;padding:8px 0;overflow-wrap:anywhere}
.warn{color:var(--f-danger);font-size:12px}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}</style></head>
<body><div id="wrap">
<h1>Certificate</h1>
<p class="sub" id="hostline"></p>
<div id="verdict" role="status"></div>
<div id="body"><p class="sub">Checking…</p></div>
</div>
<script>
(function(){
  function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}
  function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');}
  function row(k,v,copy){
    return '<div class="f"><span class="k">'+esc(k)+'</span><span class="v">'+esc(v)+'</span>'
      +(copy?'<button data-copy="'+esc(v)+'">Copy</button>':'')+'</div>';
  }
  function certBlock(c,open){
    var h='<details'+(open?' open':'')+'><summary>'+esc(c.subject)+'</summary>';
    h+=row('Issuer',c.issuer)+row('Serial',c.serial,true)+row('Valid from',c.not_before)+row('Valid until',c.not_after)
      +row('Signature',c.sig_alg)+row('Public key',c.key_alg+' '+c.key_bits+'-bit')+row('SHA-256',c.sha256,true);
    h+='<div class="f"><span class="k">SANs</span><span class="v">'+c.sans.map(esc).join('<br>')
      +(c.san_suspicious?'<br><span class="warn">Non-ASCII names present, verify the punycode form out-of-band.</span>':'')+'</span></div>';
    return h+'</details>';
  }
  window.__ferriteCerts=function(r){
    r=r||{};
    document.getElementById('hostline').textContent=r.host||'';
    var v=document.getElementById('verdict'),b=document.getElementById('body');
    if(!r.leaf){
      v.innerHTML='<span class="badge bad">'+esc(r.verify_error||'No certificate')+'</span>';
      b.innerHTML='<p class="sub">Nothing to inspect, the handshake failed or the page is not encrypted.</p>';
      return;
    }
    v.innerHTML=r.verified
      ?'<span class="badge">Trusted here</span> <span class="sub">Verified against built-in roots by a fresh handshake to this host (not the tab\u2019s own connection).</span>'
      :'<span class="badge bad">Not trusted here</span> <span class="sub">'+esc(r.verify_error||'')+'</span>';
    var h='';
    if(r.exception_trusted){
      var until=r.exception_expires_at?new Date(r.exception_expires_at).toLocaleDateString():'';
      h+='<div class="f"><span class="k">Exception</span><span class="v">You trusted this exact certificate for this host'+(until?' until '+esc(until):'')+'. Browsing still enforces normally. This only marks your grant.</span>'
        +'<button data-untrust="'+esc(r.leaf.sha256)+'">Revoke</button></div>';
    }else if(!r.verified){
      h+='<div class="f"><span class="k">Exception</span><span class="v">Trust this exact certificate for '+esc(r.host||'')+' only (self-signed router or dev server). Never a CA grant, revocable here.</span>'
        +'<button data-trust="'+esc(r.leaf.sha256)+'">Trust this cert</button></div>';
      h+='<h2>Trust the issuer instead</h2><p class="sub">Ferrite never adds root CAs itself. That grant belongs to the OS, system-wide, with its own consent. Windows: run <b>certmgr.msc</b> and import under Trusted Root. macOS: open <b>Keychain Access</b>, import into System Roots, set to Always Trust. Linux: copy the CA file into <b>/usr/local/share/ca-certificates</b> then run <b>sudo update-ca-certificates</b>.</p>';
    }
    h+='<h2>This certificate</h2>'+certBlock(r.leaf,true);
    if(r.chain&&r.chain.length>1){
      h+='<h2>Chain</h2>';
      for(var i=1;i<r.chain.length;i++)h+=certBlock(r.chain[i],false);
    }
    b.innerHTML=h;
  };
  document.getElementById('wrap').addEventListener('click',function(e){
    var c=e.target.closest?e.target.closest('[data-copy]'):null;
    if(c){post({type:'dl_copy',text:c.getAttribute('data-copy')});return;}
    var t=e.target.closest?e.target.closest('[data-trust]'):null;
    if(t){var hh=document.getElementById('hostline').textContent;post({type:'certs_trust',host:hh,sha256:t.getAttribute('data-trust')});return;}
    var u=e.target.closest?e.target.closest('[data-untrust]'):null;
    if(u){var hh2=document.getElementById('hostline').textContent;post({type:'certs_revoke',host:hh2,sha256:u.getAttribute('data-untrust')});}
  });
  try{
    var sp=new URLSearchParams(location.search);
    var u=sp.get('url')||'';
    if(u)post({type:'certs_query',url:u});
    else document.getElementById('hostline').textContent='Open this page from a secure page\u2019s lock icon.';
  }catch(_){}
})();
</script></body></html>
"#;

/// View-source page: pre-escaped text from Rust, split into addressed
/// lines client-side. Line numbers via CSS counters (never in the DOM,
/// never copied, never spoken); permalinks via `#L45` row ids; wrap is a
/// persisted preference. Find-in-page works here for free (init scripts
/// run in every webview, internal pages included).
pub const VIEWSOURCE_HTML: &str = r#"<!DOCTYPE html><!-- ferrite://viewsource -->
<html><head><meta charset="utf-8"><title>View Source - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
#wrap{padding:24px 32px;max-width:1100px}
h1{font-size:20px;margin:0 0 4px}
.sub{color:var(--f-muted);font-size:12px;margin:0 0 8px;overflow-wrap:anywhere}
#opts{display:flex;gap:8px;align-items:center;font-size:12px;color:var(--f-muted);margin-bottom:12px}
#src{border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);padding:8px 0;font-family:var(--f-font-mono);font-size:12px;counter-reset:ln;}
.ln{display:flex;min-width:0}
.ln::before{counter-increment:ln;content:counter(ln);flex:none;width:52px;text-align:right;padding-right:12px;color:var(--f-faint);cursor:pointer;user-select:none}
.ln:hover{background:var(--f-surface-1)}
.ln:target{background:var(--f-surface-2)}
.tx{flex:1;min-width:0;white-space:pre;overflow-wrap:normal}
body.wrap .tx{white-space:pre-wrap;overflow-wrap:anywhere}
.empty{color:var(--f-faint);font-size:13px;padding:16px 8px}
.notice{background:var(--f-surface-1);border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);padding:8px 12px;font-size:12px;color:var(--f-muted);margin-bottom:12px}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}</style></head>
<body><div id="wrap">
<h1>View Source</h1>
<p class="sub" id="meta"></p>
<p class="sub">Re-fetched via GET for display, may differ from form submissions, which are never replayed.</p>
<div id="opts"><label><input type="checkbox" id="wrapbox"> Word wrap</label></div>
<div id="notice"></div>
<div id="src" role="document" aria-label="Page source"></div>
</div>
<script>
(function(){
  function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}
  var src=document.getElementById('src'),
      meta=document.getElementById('meta'),
      notice=document.getElementById('notice'),
      wrapBox=document.getElementById('wrapbox');
  function applyWrap(){if(wrapBox.checked)document.body.classList.add('wrap');else document.body.classList.remove('wrap');}
  wrapBox.addEventListener('change',function(){
    applyWrap();
    post({type:'viewsource_wrap',value:wrapBox.checked});
  });
  window.__ferriteViewSource=function(d){
    d=d||{};
    meta.textContent=(d.url||'')+(d.content_type?(', '+d.content_type):'');
    wrapBox.checked=!!d.wrap;
    applyWrap();
    notice.textContent=d.truncated?('Truncated at 1 MB, the full response is larger than the viewer cap.'):'';
    if(d.error){src.innerHTML='<div class="empty">'+String(d.error).replace(/&/g,'&amp;').replace(/</g,'&lt;')+'</div>';return;}
    var lines=String(d.escaped||'').split('\n'),h='';
    for(var i=0;i<lines.length;i++){
      h+='<div class="ln" id="L'+(i+1)+'"><span class="tx">'+lines[i]+'</span></div>';
    }
    src.innerHTML=h||'<div class="empty">Empty response.</div>';
    var m=/^#L(\d+)$/.exec(location.hash||'');
    if(m){var el=document.getElementById('L'+m[1]);if(el)el.scrollIntoView();}
  };
  src.addEventListener('click',function(e){
    var g=e.target.closest?e.target.closest('.ln'):null;
    if(!g)return;
    var id=g.getAttribute('id');
    if(id)location.hash=id;
  });
  post({type:'viewsource_query'});
})();
</script></body></html>
"#;

/// Reader page: sanitized article + own typography controls (serif/size/
/// width/light-sepia-dark, independent of the chrome theme pack). Body
/// HTML arrives DOMPurify-clean and is the only innerHTML surface among
/// the manager pages, by design and commented as such.
pub const READER_HTML: &str = r#"<!DOCTYPE html><!-- ferrite://reader -->
<html><head><meta charset="utf-8"><title>Reader - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;font-family:var(--f-font-ui)}
body{background:var(--f-bg);color:var(--f-fg);}
body[data-rt="light"]{--rbg:#ffffff;--rfg:#1a1a1a;--rmut:#6b6b6b;}
body[data-rt="sepia"]{--rbg:#f4ecd8;--rfg:#433422;--rmut:#8a7a5c;}
body[data-rt="dark"]{--rbg:#1a1a1a;--rfg:#e8e6e3;--rmut:#9a978f;}
body{background:var(--rbg);color:var(--rfg);}
#wrap{padding:24px 32px 64px;max-width:640px;margin:0 auto;}
body[data-rw="1"] #wrap{max-width:920px;}
#tools{position:sticky;top:0;display:flex;gap:6px;align-items:center;padding:10px 0;background:var(--rbg);border-bottom:1px solid var(--rmut);margin-bottom:16px;flex-wrap:wrap}
#tools button,#tools select{background:transparent;border:1px solid var(--rmut);color:var(--rfg);border-radius:var(--pk-ctl-radius,6px);padding:5px 10px;cursor:pointer;font-size:12px;font-family:inherit}
#tools button[aria-pressed="true"]{border-color:var(--f-accent);color:var(--f-accent);}
#tools .sp{flex:1;}
#title{font-size:1.7em;line-height:1.25;margin:0 0 6px;overflow-wrap:anywhere;}
#meta{color:var(--rmut);font-size:13px;margin:0 0 20px;}
#body{font-size:17px;line-height:1.7;overflow-wrap:anywhere;}
body[data-rs="1"] #body{font-family:Georgia,'Times New Roman',serif;}
#body img{max-width:100%;height:auto;}
#body figure{margin:16px 0;}
#body figcaption{color:var(--rmut);font-size:13px;}
#body a{color:inherit;}
#body pre{overflow-x:auto;background:rgba(127,127,127,.12);padding:12px;border-radius:var(--pk-card-radius,8px);font-size:13px;}
#body blockquote{border-left:3px solid var(--rmut);margin:16px 0;padding:4px 0 4px 16px;}
.empty{color:var(--rmut);font-size:14px;padding:32px 0;}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}</style></head>
<body data-rt="light" data-rs="0" data-rw="0">
<div id="wrap">
<div id="tools" role="toolbar" aria-label="Reading options">
<button id="smaller" title="Smaller text">A−</button>
<button id="bigger" title="Larger text">A+</button>
<button id="serifbtn" aria-pressed="false" title="Serif typeface">Serif</button>
<button id="widebtn" aria-pressed="false" title="Wide column">Wide</button>
<button id="translatebtn" title="Translate article">Translate</button>
<span class="sp"></span>
<select id="theme" aria-label="Reading theme"><option value="light">Light</option><option value="sepia">Sepia</option><option value="dark">Dark</option></select>
</div>
<article id="art">
<h1 id="title"></h1>
<p id="meta"></p>
<div id="body"></div>
</article>
<div id="empty" class="empty" style="display:none;">Couldn’t extract an article from this page.</div>
</div>
<script>/*__FERRITE_TRANSLATE__*/</script>
<script>
(function(){
  function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}
  var prefs={size:100,serif:false,wide:false,theme:'light'};
  var translateTarget='en';
  function paintTranslateBtn(){
    var b=document.getElementById('translatebtn');
    if(!b)return;
    if(!('Translator' in self)){ b.disabled=true; b.title='Translation is not available on this device.'; }
  }
  function applyPrefs(){
    document.body.dataset.rt=prefs.theme;
    document.body.dataset.rs=prefs.serif?'1':'0';
    document.body.dataset.rw=prefs.wide?'1':'0';
    document.getElementById('body').style.fontSize=(17*prefs.size/100)+'px';
    document.getElementById('serifbtn').setAttribute('aria-pressed',prefs.serif);
    document.getElementById('widebtn').setAttribute('aria-pressed',prefs.wide);
    document.getElementById('theme').value=prefs.theme;
  }
  function savePref(key,value){prefs[key]=value;applyPrefs();post({type:'reader_pref',key:key,value:value});}
  document.getElementById('smaller').addEventListener('click',function(){
    savePref('size',Math.max(87,Math.min(150,prefs.size-12)));
  });
  document.getElementById('bigger').addEventListener('click',function(){
    savePref('size',Math.max(87,Math.min(150,prefs.size+12)));
  });
  document.getElementById('serifbtn').addEventListener('click',function(){savePref('serif',!prefs.serif);});
  document.getElementById('widebtn').addEventListener('click',function(){savePref('wide',!prefs.wide);});
  document.getElementById('theme').addEventListener('change',function(e){
    var v=e.target.value;
    if(v==='light'||v==='sepia'||v==='dark')savePref('theme',v);
  });
  document.getElementById('translatebtn').addEventListener('click',function(){
    try{ if(window.__ferriteTranslate)window.__ferriteTranslate({mode:'page',target:translateTarget}); }catch(e){}
  });
  paintTranslateBtn();
  window.__ferriteReader=function(d){
    d=d||{};
    if(typeof d.translate_target==='string'&&/^[A-Za-z]{2,3}(-[A-Za-z]{4})?(-([A-Za-z]{2}|\d{3}))?$/.test(d.translate_target))translateTarget=d.translate_target;
    // innerHTML here holds DOMPurify output by construction (the only
    // innerHTML surface among internal pages, everything else uses
    // textContent). The unsanitized form never left the source tab.
    if(d.prefs){prefs.size=d.prefs.size||100;prefs.serif=!!d.prefs.serif;prefs.wide=!!d.prefs.wide;
      if(d.prefs.theme==='light'||d.prefs.theme==='sepia'||d.prefs.theme==='dark')prefs.theme=d.prefs.theme;}
    applyPrefs();
    var empty=!d.html;
    document.getElementById('empty').style.display=empty?'':'none';
    document.getElementById('art').style.display=empty?'none':'';
    if(empty)return;
    document.getElementById('title').textContent=d.title||'';
    var meta=[];
    if(d.byline)meta.push(d.byline);
    if(d.site)meta.push(d.site);
    if(d.minutes)meta.push(d.minutes+' min read');
    document.getElementById('meta').textContent=meta.join(' · ');
    document.getElementById('body').innerHTML=d.html;
  };
  post({type:'reader_query'});
})();
</script></body></html>
"#;

/// Welcome wizard: eight skippable steps writing real settings (engines,
/// privacy flags, theme) and running real imports. No shadow preferences, no
/// forced flow. Skip is always visible, Done lands on a new tab.
/// Welcome tour art: the Ferrite mark itself on step 0 (theme-aware
/// light/dark pair swapped via prefers-color-scheme, which the shell
/// pins to the active design pack), one vendored Koboyo "Original"
/// illustration per remaining step. All inlined: offline-safe, zero
/// fetches. `currentColor` tints the Koboyo art with the pack;
/// `aria-hidden` because each heading already says what its art shows.
pub const WELCOME_HTML: &str = concat!(
    r##"<!DOCTYPE html><!-- ferrite://welcome -->
<html><head><meta charset="utf-8"><title>Welcome - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
#wrap{padding:48px 32px;max-width:620px;margin:0 auto;min-height:60vh;display:flex;flex-direction:column}
#steps{flex:1}
.step{display:none}
.step.on{display:block;animation:fade .18s ease-out}
@keyframes fade{from{opacity:0}to{opacity:1}}
h1{font-size:26px;margin:0 0 8px}h2{font-size:20px;margin:0 0 8px}
p{font-size:14px;line-height:1.6;color:var(--f-fg)}
.dim{color:var(--f-muted);font-size:13px}
#prog{font-family:var(--f-font-mono);font-size:11px;color:var(--f-faint);letter-spacing:.08em;margin-bottom:16px}
#nav{display:flex;gap:8px;margin-top:28px}
button{background:var(--f-accent);border:none;color:var(--f-bg);border-radius:var(--pk-ctl-radius,6px);padding:10px 20px;cursor:pointer;font-size:14px;font-weight:700;font-family:inherit}
button.ghost{background:none;border:1px solid var(--f-border);color:var(--f-fg);font-weight:400}
button.ghost:hover{border-color:var(--f-accent)}
select,input[type=text]{background:var(--f-surface-1);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-card-radius,8px);padding:8px 10px;margin:4px;font-size:13px;font-family:inherit}
label.row{display:flex;gap:10px;align-items:flex-start;margin:10px 0;font-size:14px;cursor:pointer}
label.row input{margin-top:3px}
.srcrow{display:flex;gap:10px;align-items:center;padding:10px 0;border-bottom:1px solid var(--f-border);font-size:14px}
.srcrow .t{flex:1}
.srcrow button{padding:6px 14px;font-size:13px}
#result{font-size:13px;color:var(--f-accent);min-height:20px;margin-top:8px}
.tart{color:var(--f-muted);margin:0 0 16px}
.tart svg{width:104px;height:auto;display:block}
.brandmark svg{height:120px;width:auto}
.brandmark .mark-light{display:none}
@media (prefers-color-scheme:light){.brandmark .mark-light{display:block}.brandmark .mark-dark{display:none}}
@media (prefers-reduced-motion:reduce){.step.on{animation:none}}</style></head>
<body><div id="wrap">
<div id="steps" role="region" aria-label="Welcome">
<div class="step" data-step="0">
<div class="tart brandmark" aria-hidden="true"><span class="mark-dark">"##,
    include_str!("../../../brand/icon_on_dark.svg"),
    r##"</span><span class="mark-light">"##,
    include_str!("../../../brand/icon_on_light.svg"),
    r##"</span></div>
<h1 tabindex="-1">Welcome to Ferrite</h1>
<p>A privacy-first desktop browser. No telemetry exists here. There is nothing to opt out of.</p>
<p class="dim">Eight short steps, each optional. Skip any time.</p>
</div>
<div class="step" data-step="1">
<div class="tart" aria-hidden="true">"##,
    include_str!("../assets/suitcase.svg"),
    r##"</div>
<h2 tabindex="-1">Bring your stuff along</h2>
<p class="dim">History imports from detected browsers. Bookmarks import from an HTML or JSON export of the old browser (folders kept); passwords need its CSV export, keychains are never read directly.</p>
<div id="sources"></div>
<div class="srcrow"><span class="t">Bookmarks HTML file</span><button data-imp="bookmarks">Choose file…</button></div>
<div class="srcrow"><span class="t">Passwords CSV file</span><button id="pwbtn">Choose file…</button></div>
<div id="result" aria-live="polite"></div>
</div>
<div class="step" data-step="2">
<div class="tart" aria-hidden="true">"##,
    include_str!("../assets/magnifying-glass.svg"),
    r##"</div>
<h2 tabindex="-1">Pick a search engine</h2>
<p class="dim">This becomes the default everywhere, omnibox, new tab, selection search.</p>
<p><select id="engine" aria-label="Default search engine"></select></p>
</div>
<div class="step" data-step="3">
<div class="tart" aria-hidden="true">"##,
    include_str!("../assets/shield-check.svg"),
    r##"</div>
<h2 tabindex="-1">Tracking protection</h2>
<label class="row"><input type="radio" name="prot" value="standard" checked><span><b>Standard</b><br><span class="dim">Block ads and trackers.</span></span></label>
<label class="row"><input type="radio" name="prot" value="strict"><span><b>Strict</b><br><span class="dim">Standard, plus HTTPS upgrades and the do-not-sell signal.</span></span></label>
</div>
<div class="step" data-step="4">
<div class="tart" aria-hidden="true">"##,
    include_str!("../assets/person-picking-theme.svg"),
    r##"</div>
<h2 tabindex="-1">Make it yours</h2>
<p class="dim">Eight design packs, light and dark. Switch any time with F4.</p>
<p><select id="packsel" aria-label="Design pack"></select></p>
</div>
<div class="step" data-step="5">
<div class="tart" aria-hidden="true">"##,
    include_str!("../assets/sync.svg"),
    r##"</div>
<h2 tabindex="-1">Take Ferrite with you</h2>
<p class="dim">Sync pairs your devices directly over the local network. No accounts, no cloud, both devices online together.</p>
<p><a href="ferrite://settings#sync"><button>Open sync settings</button></a></p>
<p class="dim">Leaving the tour replays it next launch, until you press Done.</p>
</div>
<div class="step" data-step="6">
<div class="tart" aria-hidden="true">"##,
    include_str!("../assets/house.svg"),
    r##"</div>
<h2 tabindex="-1">Default browser</h2>
<p class="dim" id="defcopy"></p>
<p><button id="defbtn">Open default-apps settings</button></p>
</div>
<div class="step" data-step="7">
<div class="tart" aria-hidden="true">"##,
    include_str!("../assets/person-launching-rocket.svg"),
    r##"</div>
<h2 tabindex="-1">Ready to browse</h2>
<p>That is everything. Ferrite opens on a blank new tab, the rest (shield details, reader mode, the password vault) introduces itself the first time it matters.</p>
</div>
</div>
<div id="prog" aria-live="polite"></div>
<div id="nav">
<button id="back" class="ghost">Back</button>
<button id="next">Next</button>
<button id="skip" class="ghost">Skip</button>
</div>
</div>
<script>
(function(){
  var step=0,last=7;
  function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}
  function postP(key,value){try{window.ipc.postMessage(JSON.stringify({type:'settings',key:key,value:value}));}catch(e){}}
  function show(n){
    step=Math.max(0,Math.min(last,n));
    document.querySelectorAll('.step').forEach(function(el){
      var on=+el.getAttribute('data-step')===step;
      if(on)el.classList.add('on');else el.classList.remove('on');
    });
    document.getElementById('prog').textContent='Step '+(step+1)+' of '+(last+1);
    document.getElementById('back').style.display=step?'':'none';
    document.getElementById('next').textContent=step===last?'Done':'Next';
    var h=document.querySelector('.step[data-step="'+step+'"] h1,.step[data-step="'+step+'"] h2');
    if(h)h.focus();
    document.getElementById('next').onclick=function(){
      if(step===last){post({type:'welcome_done'});}
      else show(step+1);
    };
  }
  document.getElementById('back').onclick=function(){show(step-1);};
  document.getElementById('skip').onclick=function(){post({type:'welcome_done'});};
  // Import sources (detected) + results. The OS default browser, when
  // named by the shell, sorts first with a badge (one-click import for
  // the common case; the full list stays visible for the rest).
  window.__ferriteWelcomeDetect=function(d){
    d=d||{};
    var srcs=(d.sources||[]).slice();
    if(d.preferred){
      srcs.sort(function(a,b){
        var ap=(a.id===d.preferred||a.id.indexOf(d.preferred)===0)?0:1;
        var bp=(b.id===d.preferred||b.id.indexOf(d.preferred)===0)?0:1;
        return ap-bp;
      });
    }
    var h='';
    srcs.forEach(function(s){
      var badge=(d.preferred&&(s.id===d.preferred||s.id.indexOf(d.preferred)===0))?' <span class="dim">(current default)</span>':'';
      h+='<div class="srcrow"><span class="t">'+s.label+badge+'</span><button data-imp="'+s.id+'">Import</button></div>';
    });
    if(!h)h='<p class="dim">No other browsers detected, you can still import files below.</p>';
    document.getElementById('sources').innerHTML=h;
    document.getElementById('defcopy').textContent=(d.os==='windows')
      ?'Windows will open its Default Apps settings. Select Ferrite there to finish. One honest hand-off, not a fake one-click.'
      :'Ferrite cannot set itself as default on this platform yet, use the operating system\u2019s own default-apps settings.';
    document.getElementById('defbtn').style.display=(d.os==='windows')?'':'none';
  };
  window.__ferriteWelcomeResult=function(r){
    r=r||{};
    var msg='';
    if(r.imported)msg+='Imported '+r.imported+' '+(r.kind==='passwords'?'passwords':r.kind)+'.';
    if(r.skipped)msg+=(msg?' ':'')+r.skipped+' already saved, skipped.';
    if(r.conflicts)msg+=(msg?' ':'')+r.conflicts+' differ from stored entries (kept). Resolve at ferrite://passwords.';
    if(r.note)msg+=(msg?' ':'')+String(r.note);
    document.getElementById('result').textContent=msg||'Nothing imported, the file may be empty or unreadable.';
  };
  document.getElementById('steps').addEventListener('click',function(e){
    var b=e.target.closest?e.target.closest('[data-imp]'):null;
    if(b)post({type:'welcome_import',kind:b.getAttribute('data-imp')});
  });
  document.getElementById('pwbtn').addEventListener('click',function(){post({type:'welcome_passwords'});});
  document.getElementById('defbtn').addEventListener('click',function(){post({type:'welcome_default_browser'});});
  // Engines, packs + current privacy values ride the shared settings push.
  window.__ferriteSetSettings=function(s){
    s=s||{};
    var eng=document.getElementById('engine');eng.textContent='';
    (s.engines||[]).forEach(function(e){
      var o=document.createElement('option');o.value=e.id;o.textContent=e.name;
      if(e.current)o.selected=true;eng.appendChild(o);
    });
    var pk=document.getElementById('packsel');
    if(pk){
      pk.textContent='';
      (s.packs||[]).forEach(function(e){
        var o=document.createElement('option');o.value=e.id;o.textContent=e.name;
        if(e.current)o.selected=true;pk.appendChild(o);
      });
    }
    var p=s.privacy||{};
    var strict=p.adblock_enabled&&p.https_upgrade_enabled&&p.gpc_enabled;
    document.querySelector('input[name="prot"][value="'+(strict?'strict':'standard')+'"]').checked=true;
  };
  document.getElementById('engine').addEventListener('change',function(e){
    post({type:'select_engine',id:e.target.value});
  });
  document.getElementById('packsel').addEventListener('change',function(e){
    post({type:'select_pack',id:e.target.value});
  });
  document.querySelectorAll('input[name="prot"]').forEach(function(r){
    r.addEventListener('change',function(){
      if(r.value==='strict'){postP('adblock',true);postP('https_up',true);postP('gpc',true);}
      else postP('adblock',true);
    });
  });
  // Every choice writes the real setting, no shadow onboarding copy.
  post({type:'settings_query'});
  post({type:'welcome_detect'});
  show(0);
})();
</script></body></html>
"##
);

/// Profile picker (boot): shown when several profiles exist and no
/// `--profile` picked one. Rows switch by spawning a sibling process
/// (side-by-side windows, Chrome model); Default continues in place,
/// Incognito enters ephemeral mode, Quit exits cleanly. The "ask every
/// time" checkbox writes the real setting.
pub const PROFILES_HTML: &str = r#"<!DOCTYPE html><!-- ferrite://profiles -->
<html><head><meta charset="utf-8"><title>Choose Profile - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
#wrap{padding:48px 32px;max-width:560px;margin:0 auto}
h1{font-size:24px;margin:0 0 6px}
p.dim{color:var(--f-muted);font-size:13px;margin:0 0 20px}
.row{display:flex;gap:12px;align-items:center;padding:12px;border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);margin:0 0 8px;cursor:pointer;font-size:14px;background:none;color:var(--f-fg);width:100%;box-sizing:border-box;text-align:left;font-family:inherit}
button.row:hover{border-color:var(--f-accent)}
.row .av{width:28px;height:28px;border-radius:50%;background:var(--f-surface-2);color:var(--f-muted);font-size:14px;font-weight:700;display:flex;align-items:center;justify-content:center;flex:none}
.row .nm{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.row .tag{font-size:11px;color:var(--f-faint);flex:none}
.row.cur{border-color:var(--f-accent)}
#foot{display:flex;gap:8px;margin-top:20px;flex-wrap:wrap}
button.act{background:none;border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-ctl-radius,6px);padding:9px 18px;cursor:pointer;font-size:14px;font-family:inherit}
button.act:hover{border-color:var(--f-accent)}
button.act.primary{background:var(--f-accent);border:none;color:var(--f-bg);font-weight:700}
label.ask{display:flex;gap:8px;align-items:center;margin-top:20px;font-size:13px;color:var(--f-muted);cursor:pointer}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}</style></head>
<body><div id="wrap">
<h1>Who is browsing?</h1>
<p class="dim">Each profile keeps its own history, passwords, tabs, and settings. Switching opens a new window; this one closes.</p>
<div id="list" role="listbox" aria-label="Profiles"></div>
<div id="foot">
<button class="act primary" id="godef">Continue with Default</button>
<button class="act" id="goinc">Browse Incognito</button>
<button class="act" id="goquit">Quit</button>
</div>
<label class="ask"><input type="checkbox" id="askalways" checked> Ask every time</label>
</div>
<script>
(function(){
  function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}
  function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');}
  window.__ferriteProfiles=function(items){
    var el=document.getElementById('list'); el.textContent='';
    (items||[]).forEach(function(p){
      var b=document.createElement('button');
      b.className='row'+(p.current?' cur':''); b.setAttribute('role','option');
      var av=document.createElement('span'); av.className='av'; av.textContent=p.initial||'?'; b.appendChild(av);
      var nm=document.createElement('span'); nm.className='nm'; nm.textContent=p.name||p.id; b.appendChild(nm);
      if(p.current){var t=document.createElement('span');t.className='tag';t.textContent='this window';b.appendChild(t);}
      b.onclick=(function(id){return function(){post({type:'profile_pick',id:id});};})(p.id);
      el.appendChild(b);
    });
  };
  document.getElementById('godef').addEventListener('click',function(){post({type:'profile_default'});});
  document.getElementById('goinc').addEventListener('click',function(){post({type:'profile_incognito'});});
  document.getElementById('goquit').addEventListener('click',function(){post({type:'profile_quit'});});
  document.getElementById('askalways').addEventListener('change',function(){post({type:'profile_picker',value:this.checked});});
  post({type:'profiles_query'});
})();
</script></body></html>
"#;

/// Crash restore page (second consecutive crash): per-tab checkboxes,
// culprit hint, diagnostics copy. Actions first in tab order so keyboard
// users never tab through dozens of rows to reach Restore.
pub const RESTORE_HTML: &str = r#"<!DOCTYPE html><!-- ferrite://restore -->
<html><head><meta charset="utf-8"><title>Restore Session - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
#wrap{padding:32px;max-width:760px}
h1{font-size:22px;margin:0 0 6px}
.sub{color:var(--f-muted);font-size:13px;margin:0 0 6px;overflow-wrap:anywhere}
#hint{font-size:12px;color:var(--f-faint);margin:0 0 16px;font-family:var(--f-font-mono);overflow-wrap:anywhere}
#actions{display:flex;gap:8px;margin:0 0 8px;flex-wrap:wrap}
button{background:var(--f-accent);border:none;color:var(--f-bg);border-radius:var(--pk-ctl-radius,6px);padding:9px 18px;cursor:pointer;font-size:14px;font-weight:700;font-family:inherit}
button.ghost{background:none;border:1px solid var(--f-border);color:var(--f-fg);font-weight:400}
button.ghost:hover{border-color:var(--f-accent)}
button:disabled{opacity:.4;cursor:default}
#tools{display:flex;gap:16px;align-items:center;font-size:13px;margin:8px 0;color:var(--f-muted);flex-wrap:wrap}
#tools label{display:flex;gap:6px;align-items:center;cursor:pointer}
#tools a{color:var(--f-accent)}
.row{display:flex;gap:10px;align-items:flex-start;padding:9px 4px;border-bottom:1px solid var(--f-border);font-size:13px;cursor:pointer}
.row:hover{background:var(--f-surface-1)}
.row input{margin-top:3px;flex:none}
.row .t{flex:1;min-width:0}
.row .t .ti{display:block;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.row .t .u{color:var(--f-faint);font-size:11px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;display:block}
.badge{font-size:10px;letter-spacing:.06em;text-transform:uppercase;color:var(--f-accent);border:1px solid var(--f-accent);border-radius:var(--pk-ctl-radius,6px);padding:1px 6px;flex:none}
.empty{color:var(--f-faint);font-size:13px;padding:16px 4px}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}</style></head>
<body><div id="wrap">
<h1 tabindex="-1">Ferrite didn’t shut down properly</h1>
<p class="sub">Pick what to bring back. Nothing reloads until you choose, unchecking a tab you suspect leaves it out for good.</p>
<p id="hint"></p>
<div id="actions">
<button id="restoreSel" disabled>Restore selected</button>
<button id="restoreAll">Restore all</button>
<button id="fresh" class="ghost">Start fresh</button>
<button id="rollbackBtn" class="ghost" style="display:none">Roll back to previous version</button>
</div>
<div id="tools">
<label><input type="checkbox" id="selall" checked> Select all</label>
<label><input type="checkbox" id="always"> Always restore my previous session</label>
<button id="diag" class="ghost">Copy diagnostics</button>
<a href="ferrite://about">About this build</a>
</div>
<div id="list" role="group" aria-label="Previous tabs"></div>
</div>
<script>
(function(){
  var list=document.getElementById('list'),selBtn=document.getElementById('restoreSel'),
      selAll=document.getElementById('selall'),tabs=[],activeIdx=-1;
  function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}
  function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');}
  function checkedUrls(){
    var out=[];
    list.querySelectorAll('input[data-url]:checked').forEach(function(c){out.push(c.getAttribute('data-url'));});
    return out;
  }
  function refreshSel(){
    var n=checkedUrls().length;
    selBtn.disabled=!n;
    selBtn.textContent=n?('Restore selected ('+n+')'):'Restore selected';
    var boxes=list.querySelectorAll('input[data-url]');
    selAll.checked=boxes.length>0&&n===boxes.length;
  }
  function render(){
    var h='';
    tabs.forEach(function(t,i){
      h+='<label class="row"><input type="checkbox" data-url="'+esc(t.url)+'" checked aria-label="'+esc(t.title||t.url)+'">'
        +'<span class="t"><span class="ti">'+esc(t.title||t.url)+'</span><span class="u">'+esc(t.url)+'</span></span>'
        +(i===activeIdx?'<span class="badge" title="Active when Ferrite closed">was active</span>':'')+'</label>';
    });
    list.innerHTML=h||'<div class="empty">Nothing to restore, the last session was already empty.</div>';
    refreshSel();
  }
  window.__ferriteRestore=function(d){
    d=d||{};
    tabs=d.tabs||[];activeIdx=(typeof d.active==='number')?d.active:-1;
    document.getElementById('hint').textContent=d.hint
      ?((d.genuine?'Looks like a real crash: ':'Closed unexpectedly: ')+d.hint)
      :'';
    // Previous-version rollback is offered only when the updater retained
    // one (single generation); otherwise the button stays hidden rather
    // than failing on click.
    document.getElementById('rollbackBtn').style.display=d.can_rollback?'':'none';
    render();
  };
  list.addEventListener('change',refreshSel);
  selAll.addEventListener('change',function(){
    list.querySelectorAll('input[data-url]').forEach(function(c){c.checked=selAll.checked;});
    refreshSel();
  });
  selBtn.addEventListener('click',function(){
    var urls=checkedUrls();
    if(urls.length)post({type:'restore_selected',urls:urls});
  });
  document.getElementById('restoreAll').addEventListener('click',function(){post({type:'restore_all'});});
  document.getElementById('fresh').addEventListener('click',function(){post({type:'restore_fresh'});});
  document.getElementById('rollbackBtn').addEventListener('click',function(){
    if(confirm('Roll back to the previous version and restart?'))post({type:'restore_rollback'});
  });
  document.getElementById('diag').addEventListener('click',function(){post({type:'restore_diag'});});
  document.getElementById('always').addEventListener('change',function(e){
    post({type:'restore_always',value:e.target.checked});
  });
  document.querySelector('h1').focus();
  post({type:'restore_data'});
})();
</script></body></html>
"#;

/// Malware interstitial. Fully local: no subresources, no IPC. The shell
/// navigates tabs here after the proxy kills a top-level tunnel, so the
/// page only reads its own query string (`?host=&at=`). Deliberately loud:
/// warning surfaces earn alarm color (§6 deliberate break). No proceed path
/// in v1, disabling the protection in Settings is the escape hatch.
pub const BLOCKED_HTML: &str = r##"<!DOCTYPE html><!-- ferrite://blocked -->
<html><head><meta charset="utf-8"><title>Blocked by Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
#wrap{padding:32px;max-width:680px}
.card{border:1px solid var(--f-border);border-left:6px solid #c93a3a;border-radius:var(--pk-card-radius,8px);padding:20px 22px;background:var(--f-surface-1)}
h1{font-size:22px;margin:0 0 4px;display:flex;gap:10px;align-items:center}
h1 svg{flex:none}
.host{font-family:var(--f-font-mono);font-size:13px;color:var(--f-muted);margin:0 0 14px;overflow-wrap:anywhere}
p{font-size:14px;line-height:1.55;margin:0 0 12px}
p.small{font-size:12px;color:var(--f-faint)}
#actions{display:flex;gap:8px;margin:16px 0 4px;flex-wrap:wrap}
button{background:var(--f-accent);border:none;color:var(--f-bg);border-radius:var(--pk-ctl-radius,6px);padding:9px 18px;cursor:pointer;font-size:14px;font-weight:700;font-family:inherit}
button.ghost{background:none;border:1px solid var(--f-border);color:var(--f-fg);font-weight:400}
button.ghost:hover{border-color:var(--f-accent)}
details{font-size:12px;color:var(--f-muted);margin-top:12px}
details summary{cursor:pointer}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}</style></head>
<body><div id="wrap"><div class="card">
<h1 tabindex="-1"><svg width="26" height="26" viewBox="0 0 16 16" fill="none" stroke="#c93a3a" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M8 1.5 15 13.5H1L8 1.5Z"/><line x1="8" y1="6" x2="8" y2="9.5"/><circle cx="8" cy="11.6" r=".4" fill="#c93a3a"/></svg> This site may harm your computer</h1>
<p class="host" id="host"></p>
<p id="why"></p>
<div id="actions">
<button id="back">Go back</button>
<button id="safety" class="ghost">Go somewhere safe</button>
<button id="trust" class="ghost">Trust this site</button>
<button id="report" class="ghost">Report as incorrect</button>
</div>
<details><summary>Technical details</summary>
<p class="small" id="tech"></p>
<p class="small" id="trustnote"></p>
</details>
</div></div>
<script>(function(){
  function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}
  var q=new URLSearchParams(location.search);
  var host=q.get('host')||'this site';
  var at=parseInt(q.get('at')||'0',10);
  document.getElementById('host').textContent=host;
  var when=isFinite(at)&&at>0?new Date(at).toLocaleString():'unknown time';
  document.getElementById('tech').textContent='Blocked host: '+host+' · at '+when+'.';
  function paint(d){
    d=d||{};
    var why=d.why||'malware', w=document.getElementById('why'), t=document.getElementById('trustnote');
    window.__ferriteBlockedWhy=why;
    if(why==='lookalike'&&d.matched){
      w.textContent='This address looks like '+(d.matched||'')+', a site you visit, it may be an impersonation set up to steal passwords. If you typed this yourself and know it is legitimate, you can trust it below.';
      t.textContent='Heuristic match (lookalike domain), not a confirmed blocklist entry.';
    }else if(why==='form'&&d.action){
      w.textContent='A login form on the previous page tried to send your credentials to '+(d.action||host)+', a different site. If that redirect is expected (single sign-on), you can trust this site below.';
      t.textContent='Heuristic match (cross-site credential post), not a confirmed blocklist entry.';
    }else{
      w.textContent='Ferrite blocked this page because it matches a known-malware source. Attackers on sites like this can install harmful software or steal passwords and payment details just by visiting.';
      t.textContent='Confirmed match: local blocklist or real-time lookup.';
    }
    var trust=document.getElementById('trust');
    if(d.trusted){trust.textContent='Trusted, go back and retry';trust.disabled=true;}
    else{trust.textContent='Trust this site';trust.disabled=false;}
    var rep=document.getElementById('report');
    if(d.reported){rep.textContent='Report recorded';rep.disabled=true;}
    else{rep.textContent='Report as incorrect';rep.disabled=false;}
  }
  window.__ferriteBlocked=function(d){try{paint(d);}catch(_){}};
  paint(null);
  post({type:'blocked_query',host:host});
  document.getElementById('back').addEventListener('click',function(){history.back();});
  document.getElementById('safety').addEventListener('click',function(){location.href='ferrite://newtab';});
  document.getElementById('trust').addEventListener('click',function(){post({type:'threat_trust',host:host});});
  document.getElementById('report').addEventListener('click',function(){post({type:'threat_report',host:host,why:(window.__ferriteBlockedWhy||'malware')});});
  document.querySelector('h1').focus();
})();
</script></body></html>
"##;

/// Connection-error interstitial. Fully local: no subresources, no IPC.
/// It only reads its own query string (`?url=&reason=&at=`). Reached two
/// ways: the proxy 302s failed plain-HTTP documents here, and the shell
/// navigates tabs here after a dead HTTPS tunnel. Neutral chrome, not
/// alarm: unlike malware, this is usually transient.
/// Error page art: one vendored Koboyo "Original" illustration
/// (`person-no-signal`), inlined so the page stays offline-safe with
/// zero fetches. `currentColor` tints it with the theme; `aria-hidden`
/// because the heading already says what the art shows.
pub const ERROR_HTML: &str = concat!(
    r##"<!DOCTYPE html><!-- ferrite://error -->
<html><head><meta charset="utf-8"><title>Connection failed - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
#wrap{padding:32px;max-width:680px}
.card{border:1px solid var(--f-border);border-left:6px solid var(--f-accent);border-radius:var(--pk-card-radius,8px);padding:20px 22px;background:var(--f-surface-1)}
.art{color:var(--f-muted);margin:2px 0 14px;position:relative;width:max-content}
.art svg{width:132px;height:auto;display:block}
.art .badge{position:absolute;right:-10px;bottom:-4px;color:var(--f-warning);background:var(--f-surface-1);border-radius:50%;padding:5px;line-height:0}
.art .badge svg{width:46px;height:auto}
h1{font-size:22px;margin:0 0 4px;overflow-wrap:anywhere}
.url{font-family:var(--f-font-mono);font-size:13px;color:var(--f-muted);margin:0 0 14px;overflow-wrap:anywhere}
p{font-size:14px;line-height:1.55;margin:0 0 12px}
p.small{font-size:12px;color:var(--f-faint)}
#actions{display:flex;gap:8px;margin:16px 0 4px;flex-wrap:wrap}
button{background:var(--f-accent);border:none;color:var(--f-bg);border-radius:var(--pk-ctl-radius,6px);padding:9px 18px;cursor:pointer;font-size:14px;font-weight:700;font-family:inherit}
button.ghost{background:none;border:1px solid var(--f-border);color:var(--f-fg);font-weight:400}
button.ghost:hover{border-color:var(--f-accent)}
details{font-size:12px;color:var(--f-muted);margin-top:12px}
details summary{cursor:pointer}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}</style></head>
<body><div id="wrap"><div class="card">
<div class="art" aria-hidden="true">"##,
    include_str!("../assets/person-no-signal.svg"),
    r##"<span class="badge">"##,
    include_str!("../assets/wifi-off.svg"),
    r##"</span></div>
<h1 tabindex="-1" id="title">Can’t reach this site</h1>
<p class="url" id="url"></p>
<p id="blurb">The connection failed before anything loaded. Nothing was sent. This is safe to retry.</p>
<div id="actions">
<button id="retry">Retry</button>
<button id="back" class="ghost">Go back</button>
<button id="safety" class="ghost">Go somewhere safe</button>
</div>
<details><summary>Technical details</summary>
<p class="small" id="tech"></p>
</details>
</div></div>
<script>(function(){
  var q=new URLSearchParams(location.search);
  var u=q.get('url')||'';
  var reason=q.get('reason')||'unreachable';
  var at=parseInt(q.get('at')||'0',10);
  var copy={
    refused:['refused to connect','The site actively refused the connection. It may be down, overloaded, or blocking your network.'],
    timeout:['is taking too long to respond','The server didn’t answer in time. It may be overloaded, waiting a minute often helps.'],
    unreachable:['can’t be reached','Check the address for typos. If you’re offline, reconnect and retry.'],
    server:['isn’t working right now','The server reported an internal error. That’s on their end, retry in a bit.'],
    'proxy-down':['needs its privacy proxy','Ferrite’s local proxy isn’t running, so this page stayed unloaded rather than browsing unprotected. Restart Ferrite and retry.']
  };
  var c=copy[reason]||copy.unreachable;
  var host=u;
  try{host=new URL(u).hostname||u;}catch(e){host=u||'this site';}
  document.getElementById('title').textContent=host+' '+c[0];
  document.getElementById('url').textContent=u||location.href;
  document.getElementById('blurb').textContent=c[1]+' Nothing was sent. This is safe to retry.';
  var when=isFinite(at)&&at>0?new Date(at).toLocaleString():'unknown time';
  document.getElementById('tech').textContent='Attempted: '+(u||location.href)+' · reason: '+reason+' · at '+when+'.';
  var retry=document.getElementById('retry');
  if(u){retry.addEventListener('click',function(){location.href=u;});}
  else{retry.style.display='none';}
  document.getElementById('back').addEventListener('click',function(){history.back();});
  document.getElementById('safety').addEventListener('click',function(){location.href='ferrite://newtab';});
  document.querySelector('h1').focus();
})();
</script></body></html>
"##
);

pub const CRASHED_HTML: &str = r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Tab Crashed</title>
<style>/*__FERRITE_THEME__*/html,body{margin:0;height:100%;background:var(--f-bg);color:var(--f-danger);font-family:var(--f-font-ui);display:flex;align-items:center;justify-content:center;flex-direction:column}</style>
</head><body><h1>This tab has crashed.</h1><p><button onclick="location.reload()">Reload Tab</button></p></body></html>
"#;

pub const SETTINGS_HTML: &str = r##"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Settings - Ferrite</title>
<style>/*__FERRITE_THEME__*/html,body{margin:0;padding:0;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}#top{padding:24px 32px 0}#setsearch{background:var(--f-surface-1);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-card-radius,8px);padding:8px 12px;width:min(480px,90%);font-size:14px}#wrap{display:flex;gap:32px;padding:24px 32px;max-width:1100px}#setnav{width:200px;flex:none;position:sticky;top:24px;align-self:flex-start}#setnav a{display:block;padding:8px 12px;color:var(--f-muted);text-decoration:none;border-radius:var(--pk-card-radius,8px);font-size:14px}#setnav a:hover{background:var(--f-surface-1);color:var(--f-fg)}#setbody{flex:1;min-width:0}section{margin-bottom:32px}h2{margin-top:0}.set{padding:12px 0;border-bottom:1px solid var(--f-border)}.set .t{font-weight:600}.set .d{color:var(--f-muted);font-size:13px;margin-top:4px;max-width:640px}label.row{display:flex;align-items:center;gap:8px;margin:4px 0;font-size:14px}input[type=checkbox]{accent-color:var(--f-accent)}button{margin:4px;padding:6px 12px;background:var(--f-surface-1);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-ctl-radius,6px);cursor:pointer}button:hover{border-color:var(--f-accent)}select,input[type=text]{background:var(--f-surface-1);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-card-radius,8px);padding:6px 8px;margin:4px;font-size:13px}a{color:var(--f-accent)}#customList div{display:flex;gap:8px;align-items:center;margin:4px 0;font-size:13px}#customList code{color:var(--f-muted)}@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}#recBox[hidden]{display:none}#recPhrase{font-family:var(--f-font-mono);background:var(--f-surface-1);border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);padding:12px 14px;font-size:14px;max-width:640px}#syncCodeBox[hidden]{display:none}#syncCode{font-family:var(--f-font-mono);background:var(--f-surface-1);border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);padding:8px 10px;font-size:12px;max-width:640px;width:100%;box-sizing:border-box;color:var(--f-fg)}#syncPending{border:1px solid var(--f-warning);border-radius:var(--pk-card-radius,8px);padding:10px 14px;margin:8px 0;max-width:640px}#syncPending[hidden]{display:none}#syncQr svg{width:200px;height:200px;margin-top:8px;background:#fff}</style>
</head><body>
<div id="top"><h1>Settings</h1><input id="setsearch" placeholder="Search settings" autocomplete="off" spellcheck="false"></div>
<div id="wrap"><nav id="setnav">
<a href="#cat-privacy">Privacy &amp; Security</a><a href="#cat-search">Search</a><a href="#cat-appearance">Appearance</a><a href="#cat-language">Language</a><a href="#cat-downloads">Downloads</a><a href="#cat-startup">Startup</a><a href="#cat-performance">Performance</a><a href="#cat-autofill">Autofill</a><a href="#cat-sync">Sync</a><a href="#cat-advanced">Advanced</a><a href="#cat-about">About</a>
</nav><div id="setbody">
<section id="cat-privacy"><h2>Privacy &amp; Security</h2>
<div class="set" id="set-privacy-adblock" data-keys="adblock ads block trackers easylist filter"><div class="t">Adblock (EasyList)</div><label class="row"><input type="checkbox" id="adblock" checked onchange="post('adblock',this.checked)"> Enabled</label><div class="d">Blocks ads and known trackers before they load. Very rarely breaks page layout.</div></div>
<div class="set" id="set-privacy-gpc" data-keys="gpc opt out sale share privacy signal"><div class="t">Send Sec-GPC: 1</div><label class="row"><input type="checkbox" id="gpc" checked onchange="post('gpc',this.checked)"> Enabled</label><div class="d">Tells sites you opt out of sale or sharing of your data. Legally binding in some regions, ignored in others.</div></div>
<div class="set" id="set-privacy-realtime" data-keys="real-time realtime safe browsing hash lookup threat malware phishing api key"><div class="t">Real-time hash-prefix lookup</div><label class="row"><input type="checkbox" id="realtime" onchange="post('realtime',this.checked)"> Enabled</label><div class="d">Checks truncated hash prefixes directly with Google Safe Browsing (never the proxy) under a 300ms fail-open budget. Off unless you opt in; inert without an API key. Only truncated hashes leave the device.</div><label class="row">API key <input type="text" id="realtimeKey" placeholder="Safe Browsing API key" autocomplete="off" spellcheck="false" maxlength="200"> <button onclick="saveRealtimeKey()">Save</button> <button onclick="clearRealtimeKey()">Clear</button></label><div class="d" id="realtimeKeyState"></div></div>
<div class="set" id="set-privacy-doh" data-keys="doh dns encrypted hostname endpoint query private resolver"><div class="t">Encrypted DNS (DoH)</div><label class="row">Endpoint <input type="text" id="dohEndpoint" placeholder="https://dns.example/dns-query" autocomplete="off" spellcheck="false" size="32"> <button onclick="saveDoh()">Save</button> <button onclick="clearDoh()">Clear</button></label><div class="d" id="dohState"></div><div class="d">Optional, off by default. Proxy DNS goes through this https JSON endpoint instead of your OS resolver. Invalid endpoints are refused, never probed. The endpoint's own hostname still resolves via the OS, and packet capture shows plaintext DNS on port 53 unless you opt in.</div></div>
<div class="set" id="set-privacy-https" data-keys="https upgrade tls ssl http encrypted"><div class="t">HTTPS upgrade</div><label class="row"><input type="checkbox" id="https_up" checked onchange="post('https_up',this.checked)"> Enabled</label><div class="d">Tries the encrypted endpoint first. A site with no HTTPS still loads over plain HTTP, the address bar shows it as insecure.</div></div>
<div class="set" id="set-privacy-passwords" data-keys="password manager credentials totp login"><div class="t">Password manager</div><div class="d">Lives on its own page: <a href="ferrite://passwords">ferrite://passwords</a>. The vault stays encrypted at rest; nothing is ever sent anywhere.</div></div>
<div class="set" id="set-privacy-exclusions" data-keys="excluded exclusions never save history per-site origin site exceptions"><div class="t">Excluded sites</div><div class="d">Never save history for these origins, subdomains included. Paste a URL or a bare host, only the host is stored.</div><p><input type="text" id="exOrigin" placeholder="example.com" size="24"> <button onclick="exclusionAdd()">Exclude</button></p><div id="exList"></div></div>
<div class="set" id="set-privacy-perms" data-keys="permissions camera microphone location notifications clipboard allowed always revoke grants site permissions"><div class="t">Site permissions</div><div class="d">Permanent ("always allow") grants plus origins auto-blocked after repeated unanswered prompts. One-time grants vanish after use and are never listed. Revoking takes effect on the next request, it cannot stop an already-running capture, only the next prompt.</div><div id="permGrants"></div></div>
<div class="set" id="set-privacy-notifications" data-keys="notifications push alerts sites global kill switch quiet"><div class="t">Site notifications</div><label class="row"><input type="checkbox" id="notifGlobal" checked onchange="post('notifications',this.checked)"> Enabled</label><div class="d">Master switch for website push notifications (the OS-level kind, not in-app toasts). Off denies every site silently with no prompts; per-site grants resume when re-enabled. Delivery itself runs on the engine's push relay, which Ferrite does not operate.</div></div>
<div class="set" id="set-privacy-clear" data-keys="clear browsing data history downloads cache cookies delete erase range hour day week"><div class="t">Clear browsing data</div><p><select id="clearRange"><option value="hour">Last hour</option><option value="day" selected>Last 24 hours</option><option value="week">Last 7 days</option><option value="all">All time</option></select>
<label class="row"><input type="checkbox" id="clearHistory" checked> History</label>
<label class="row"><input type="checkbox" id="clearDownloads" checked> Download records (files stay on disk)</label>
<label class="row"><input type="checkbox" id="clearCache"> Engine cache (can't be undone)</label>
<label class="row"><input type="checkbox" id="clearCookies"> Cookies, all sites and all-time (can't be undone)</label>
<div class="d">History, passwords, and autofill are encrypted at rest. Cached page content relies on your OS's disk encryption, keep BitLocker, FileVault, or LUKS on.</div>
<button onclick="clearData()">Clear selected</button></p><p id="clearOut"></p></div>
<div class="set" id="set-privacy-retention" data-keys="history retention keep delete old visits automatically expire"><div class="t">History retention</div><p><select id="historyRetention" onchange="postHistoryRetention(this.value)"><option value="30">Keep 30 days</option><option value="90">Keep 90 days</option><option value="180">Keep 6 months</option><option value="365">Keep 1 year</option><option value="0">Keep forever</option></select></p><div class="d">Older visits are deleted automatically, once a day. Applies to history only; bookmarks, passwords, and downloads records stay.</div></div>
<div class="set" id="set-privacy-cookies" data-keys="cookies site per-site third party storage reset jar"><div class="t">Site cookies</div><div class="d">What's stored for one origin, from the engine's own jar (profile-wide, every tab shares it). Reset deletes only that origin's cookies, never the whole jar. Third-party cookies are stripped at the proxy before they land; the engine sets new cookies SameSite=Lax by default.</div><p><input type="text" id="ckOrigin" placeholder="example.com" size="24"> <button onclick="cookiesQuery()">Inspect</button> <button onclick="cookiesClearSite()">Reset this site</button></p><div id="ckList"></div><p id="ckOut"></p></div>
<div class="set" id="set-privacy-rotation" data-keys="rotate rotation encryption key rekey re-encrypt master"><div class="t">Encryption key</div><div class="d">Re-encrypts every store (vault, archive, session, settings) under a fresh device key. The old key stays live until every store verifies under the new one; a crash mid-rotation resumes on next boot. Takes a moment, runs in the background, reports when done.</div><p><button onclick="postRotate()">Rotate encryption key</button></p></div>
<div class="set" id="set-privacy-recovery" data-keys="recovery phrase mnemonic backup restore key lost keyring"><div class="t">Recovery phrase</div><div class="d">A 24-word phrase that restores your encrypted data if the device key is ever lost. Generating a new phrase replaces the old one. Write it down and keep it somewhere safe. Anyone holding it can read everything.</div><p><button onclick="postRecoveryShow()">Generate new recovery phrase</button> <button onclick="postRecoveryDisable()">Disable</button></p><div id="recBox" hidden><p>Write these 24 words down now. This is the only time they are shown:</p><p id="recPhrase"></p></div></div>
</section>
<section id="cat-search"><h2>Search</h2>
<div class="set" id="set-search-engine" data-keys="search engine default duckduckgo google bing brave private window"><div class="t">Default search engine</div><p><select id="engine"></select></p><p>Private windows: <select id="enginePrivate"></select></p><div class="d">Empty follows the regular default.</div></div>
<div class="set" id="set-search-custom" data-keys="custom search engine add template query url"><div class="t">Custom search engines</div><div class="d">Use <code>%s</code> where the query goes, e.g. <code>https://s.test/?q=%s</code>. Custom engines run the same classify path as built-ins.</div><p><input type="text" id="cName" placeholder="Name" size="16"> <input type="text" id="cTpl" placeholder="https://…?q=%s" size="32"> <button onclick="customAdd()">Add</button></p><div id="customList"></div></div>
<div class="set" id="set-search-suggestions" data-keys="suggestions autocomplete omnibox keystroke remote duckduckgo"><div class="t">Autocomplete suggestions</div><label class="row"><input type="checkbox" id="suggestions" checked onchange="postSug(this.checked)"> Enabled</label><div class="d">Suggestions come from your local history only, keystrokes never leave the device, so this stays on unless you prefer a bare bar.</div><label class="row"><input type="checkbox" id="suggestions_remote" onchange="postSugRemote(this.checked)"> Remote suggestions (DuckDuckGo)</label><div class="d">Off by default. When on, each keystroke is sent to DuckDuckGo for completions (ranked below everything local, dropped silently on failure).</div></div>
<div class="set" id="set-search-home" data-keys="home homepage start page button destination"><div class="t">Home button destination</div><p><input type="text" id="homeUrl" size="40" placeholder="ferrite://newtab"> <button onclick="saveHome()">Save</button></p><div class="d">Where the toolbar home button goes. Web or ferrite:// pages only.</div></div>
</section>
<section id="cat-appearance"><h2>Appearance</h2>
<div class="set" id="set-appearance-pack" data-keys="theme appearance dark light pack volcanic tenebra"><div class="t">Theme</div><p><select id="pack"></select></p><div class="d">Eight design packs. F4 cycles them without opening this page.</div></div>
<div class="set" id="set-appearance-tabs" data-keys="tabs vertical horizontal strip rail sidebar layout"><div class="t">Vertical tabs</div><label class="row"><input type="checkbox" id="tabsVertical" onchange="post('tabs_vertical',this.checked)"> Dock the tab strip to the left rail</label><div class="d">Moves tabs (and group headers) into a left rail; the toolbar stays on top and pages shift right. Same tabs, same shortcuts, same collapsing groups.</div></div>
</section>
<section id="cat-language"><h2>Language</h2>
<div class="set" id="set-language-spellcheck" data-keys="spellcheck spelling suggestions hunspell dictionary dictionaries red squiggle text fields misspelled"><div class="t">Spelling suggestions</div><label class="row"><input type="checkbox" id="spellcheck" checked onchange="post('spellcheck',this.checked)"> Enabled</label><div class="d">Inline spelling suggestions (the red squiggle) in text fields. Checking is the native engine, Hunspell dictionaries ship bundled and are never fetched over the network; on macOS the system checker is used instead. Right-click a misspelled word for suggestions and Add to Dictionary (words you add live in the engine profile). Fields the page marked non-checkable stay unchecked.</div></div>
<div class="set" id="set-language-langs" data-keys="spellcheck languages list codes tags dictionary active consult"><div class="t">Spellcheck languages</div><p><input type="text" id="spellLangs" size="40" placeholder="en-US"> <button onclick="saveSpellLangs()">Save</button></p><div class="d">Which language dictionaries are active, as a comma-separated list of language codes (e.g. en-US, de). Applied as language tags the native checker consults; detection itself stays automatic and multilingual.</div></div>
<div class="set" id="set-language-translate" data-keys="translate translation on-device local language page selection translator"><div class="t">On-device translation</div><label class="row"><input type="checkbox" id="translate" checked onchange="post('translate_enabled',this.checked)"> Enabled</label><div class="d">Translate pages and selections with the engine built-in translator. Runs fully on this device once the language model is downloaded; page text is never sent anywhere. Needs a capable device, otherwise pages report that translation is unavailable. Never shows which language packs are stored, probing that would fingerprint you.</div></div>
<div class="set" id="set-language-translate-target" data-keys="translate target language destination default translator"><div class="t">Translation target language</div><p><input type="text" id="translateTarget" size="12" placeholder="en"> <button onclick="saveTranslateTarget()">Save</button></p><div class="d">The language pages and selections translate into, as a language code (e.g. en, de, fr, pt-BR). Source language is detected on-device automatically.</div></div>
</section>
<section id="cat-downloads"><h2>Downloads</h2>
<div class="set" id="set-downloads-page" data-keys="downloads folder save files library"><div class="t">Download library</div><div class="d">Files save to your Downloads folder. Browse, verify, and manage them at <a href="ferrite://downloads">ferrite://downloads</a>. Clearing records never deletes files.</div><label class="row"><input type="checkbox" id="askDownloads" onchange="postAskDownloads(this.checked)"> Ask where to save each download</label><div class="d">Off by default. When on, every download shows a save dialog first (form-button downloads can't survive the handoff and stay automatic).</div></div>
</section>
<section id="cat-startup"><h2>Startup</h2>
<div class="set" id="set-startup-mode" data-keys="startup start launch open restore session new tab pages on startup"><div class="t">On startup</div><p><select id="startupMode" onchange="postStartupMode(this.value)"><option value="restore">Reopen previous tabs</option><option value="newtab">Open a new tab</option><option value="urls">Open specific pages</option></select></p><div class="d">Clean launches follow this. Crashes keep their own offer (or silent restore with "Always restore on launch"), and an explicit URL on the command line always opens in addition.</div></div>
<div class="set" id="set-startup-urls" data-keys="startup pages urls list open specific sites launch"><div class="t">Startup pages</div><div class="d">Used only by "Open specific pages" above (up to 10). The last opens active.</div><p><input type="text" id="startupUrl" placeholder="https://…" size="32"> <button onclick="startupUrlAdd()">Add page</button></p><div id="startupList"></div></div>
<div class="set" id="set-startup-picker" data-keys="startup profile picker ask who browsing choose launch"><div class="t">Profile picker</div><label class="row"><input type="checkbox" id="profilePicker" checked onchange="post('profile_picker',this.checked)"> Ask which profile at startup</label><div class="d">Only when several profiles exist and no --profile flag picked one. Switching spawns a side-by-side window.</div></div>
</section>
<section id="cat-performance"><h2>Performance</h2>
<div class="set" id="set-performance-sleep" data-keys="sleep sleeping tabs inactive unload discard memory timeout performance"><div class="t">Sleep inactive tabs</div><p><select id="sleepTimeout" onchange="postSleepTimeout(this.value)"><option value="0">Never</option><option value="5">After 5 minutes</option><option value="15">After 15 minutes</option><option value="30">After 30 minutes</option><option value="60">After 1 hour</option><option value="120">After 2 hours</option><option value="240">After 4 hours</option><option value="480">After 8 hours</option></select></p><div class="d">Background tabs past the timeout unload their renderer and free memory; click wakes them with history intact. Tabs playing audio, holding unsaved forms, on calls, or pinned (until memory runs hot) never sleep on their own. Right-click any tab for Sleep now.</div></div>
<div class="set" id="set-performance-exceptions" data-keys="sleep exceptions never sleep keep awake sites whitelist memory"><div class="t">Never sleep these sites</div><div class="d">Subdomains included: listing example.com keeps mail.example.com awake. Manual Sleep tab still works on listed sites.</div><p><input type="text" id="sleepHost" placeholder="example.com" size="24"> <button onclick="sleepExceptionAdd()">Keep awake</button></p><div id="sleepList"></div></div>
</section>
<section id="cat-autofill"><h2>Autofill</h2>
<div class="set" id="set-autofill-address" data-keys="autofill address form fill street postal"><div class="t">Address autofill</div><label class="row"><input type="checkbox" id="autofillAddress" checked onchange="postAutofill('autofill_address',this.checked)"> Enabled</label><div class="d">Offer saved addresses on focus or right-click in detected fields. Fills the clicked field per explicit pick, never silently. Manage them at <a href="ferrite://autofill">ferrite://autofill</a>.</div></div>
<div class="set" id="set-autofill-payment" data-keys="autofill payment card credit fill checkout"><div class="t">Payment autofill</div><label class="row"><input type="checkbox" id="autofillPayment" checked onchange="postAutofill('autofill_payment',this.checked)"> Enabled</label><div class="d">Offer saved cards on focus or right-click the same way. CVV is never stored or filled. Always typed by hand.</div></div>
<div class="set" id="set-autofill-form" data-keys="autofill form history remember typed values per-origin"><div class="t">Form history</div><label class="row"><input type="checkbox" id="autofillForm" checked onchange="postAutofill('autofill_form',this.checked)"> Enabled</label><div class="d">Remember what you type in ordinary fields, scoped per origin (never shared across sites). Passwords, card numbers, and sensitive-looking fields are never recorded, turning this off wipes what's stored.</div></div>
</section>
<section id="cat-sync"><h2>Sync</h2>
<div class="set" id="set-sync-devices" data-keys="sync devices pair pairing trusted revoke device mesh trust code qr scan categories history live tabs network"><div class="t">Trusted devices</div><div class="d">Devices you have paired with this one. Pairing binds keys with your explicit approval; revoking cuts a device at the next handshake. Without a relay, sync runs over the local network only and both devices must be online together. Live tabs from paired devices appear below. Opening one loads it here. Codes exchanged by hand never touch the network. Categories choose what syncs (history stays off until you opt in); trusted-device changes always sync so revocation propagates.</div><p><button onclick="syncRefresh()">Refresh</button> <button onclick="syncCodeShow()">Show my pairing code</button></p><div id="syncStatus"></div><div id="syncSelf"></div><div id="syncList"></div><div id="syncCats"></div><div id="syncPeers"></div><div id="syncLive"></div><div id="syncCodeBox" hidden><p>Copy this code to the other device, then approve the pairing there, or scan the QR code with Ferrite on your phone:</p><p><textarea id="syncCode" rows="3" readonly></textarea></p><div class="d" id="syncCodeLeft"></div><div id="syncQr"></div></div><p><input type="text" id="pairCode" placeholder="Paste a pairing code" size="40"> <button onclick="syncPairSubmit()">Review pairing</button></p><div id="syncPending" hidden></div><div class="set" id="set-sync-relay" data-keys="sync relay off-lan server remote address"><div class="t">Off-LAN relay</div><div class="d">Optional. Without it, sync only reaches devices on this local network. With it, paired devices sync anywhere, through ciphertext the relay cannot read. A bounded store-and-forward mailbox holds addressed ciphertext for offline devices and expires it. Leave empty for LAN-only.</div><p><input type="text" id="relayUrls" placeholder="wss://relay.example.com/sync" size="40"> <button onclick="syncRelaySave()">Save</button> <button onclick="syncRelayClear()">Clear</button></p><div class="d" id="relayState"></div></div>
</section>
<section id="cat-advanced"><h2>Advanced</h2>
<div class="set" id="set-advanced-recovery" data-keys="recovery phrase vault backup mnemonic"><div class="t">Vault recovery</div><div class="d">Same controls as the Privacy section: generating a new phrase replaces the old one. The phrase is shown once, in the Privacy section above. Write it down and keep it somewhere safe.</div><p><button onclick="postRecoveryShow()">Generate new recovery phrase</button> <button onclick="postRecoveryDisable()">Disable</button></p></div>
<div class="set" id="set-advanced-transfer" data-keys="export import settings file backup move device sync"><div class="t">Move settings to another device</div><div class="d">Exports preferences as plain JSON for manual transfer between devices. The file includes secrets (Safe Browsing key, DoH endpoint). Store it safely. Importing merges over current settings after validation.</div><p><button onclick="exportSettings()">Export settings file</button> <button onclick="importSettings()">Import settings file</button></p></div>
<div class="set" id="set-advanced-export" data-keys="export data backup portable tar encrypted device migration download takeout"><div class="t">Export my data</div><div class="d">Per-category files stay where they are (bookmarks page, passwords page, settings above). Everything at once, or one encrypted backup for a new device. Never both in one file:</div><p><button id="exportAllBtn" onclick="exportAllArm()">Export everything (.tar)</button> <button onclick="backupExport()">Back up encrypted (.enc)</button> <button onclick="backupImport()">Restore backup…</button></p><p><input type="password" id="backupPass" placeholder="Backup passphrase (8+ characters)" autocomplete="new-password" size="32"></p><p class="sub" id="exportOut" aria-live="polite"></p></div>
<div class="set" id="set-advanced-imports" data-keys="import bookmarks history chrome firefox netscape"><div class="t">Import browsing data</div><div class="d">Bookmarks keep their folders (HTML, Chromium or Firefox JSON). History keeps its original dates. Repeats are safe: already-saved entries are skipped, never duplicated.</div><p><button onclick="importBookmarks()">Import Bookmarks (HTML, JSON)</button> <button onclick="importHistory()">Import History (Chrome/Firefox DB)</button> <button onclick="exportData()">Export bookmarks (HTML)</button> <button onclick="exportHistory()">Export history (CSV)</button></p></div>
<div class="set" id="set-advanced-reset" data-keys="reset defaults restore factory wipe"><div class="t">Reset everything</div><div class="d">Restores factory defaults and deletes the settings file. History, vault, and downloads are untouched, this only resets preferences.</div><p><button onclick="if(confirm('Reset all settings to defaults?'))resetSettings()">Reset all settings…</button></p><p id="advOut"></p></div>
<div class="set" id="set-advanced-restore" data-keys="restore session startup crash relaunch tabs"><div class="t">Restore previous session</div><label class="row"><input type="checkbox" id="restoreAlways" onchange="postRestore(this.checked)"> Always restore on launch</label><div class="d">Clean launches reopen your tabs silently either way. With this on, a crash restores automatically too, the crash bar never appears.</div></div>
<div class="set" id="set-advanced-pip" data-keys="picture in picture pip video floating window auto"><div class="t">Picture-in-Picture</div><label class="row"><input type="checkbox" id="pipAuto" onchange="postPip(this.checked)"> Automatically float videos when switching tabs</label><div class="d">Off by default. When on, switching away from a tab with a playing video floats it in an always-on-top window. The omnibox PiP button always works regardless, this only controls the automatic pop-out. The floating window is provided by the OS engine and stays visible above every app until its tab closes or navigates.</div></div>
</section>
<section id="cat-about"><h2>About</h2>
<div class="set" id="set-about-version" data-keys="about version update channel changelog"><div class="t">Ferrite <span id="ver">/*__FERRITE_VERSION__*/</span></div><div class="d">A privacy-first desktop browser. No telemetry exists in this product. There is nothing to opt out of. See <a href="ferrite://about">About Ferrite</a> for version details and every internal page.</div></div>
<div class="set" id="set-about-oss" data-keys="open source license attribution third party phosphor koboyo"><div class="t">Open-source attributions</div><div class="d">Phosphor Icons (MIT) · Koboyo Icons (free commercial) · wry / winit / tokio / rusqlite (MIT / Apache-2.0) · Hunspell dictionaries (LGPL/GPL/MPL, bundled so spellchecking never fetches). Full dependency licenses ship in the repository.</div></div>
</section>
</div></div>
<script>
function post(key, value){ try{ window.ipc.postMessage(JSON.stringify({type:'settings',key,value})); }catch(e){} }
function postSug(v){ try{ window.ipc.postMessage(JSON.stringify({type:'suggestions',value:v})); }catch(e){} }
function postSugRemote(v){ try{ window.ipc.postMessage(JSON.stringify({type:'suggestions_remote',value:v})); }catch(e){} }
function postRotate(){ try{ window.ipc.postMessage(JSON.stringify({type:'rotate_key'})); }catch(e){} }
function postRestore(v){ try{ window.ipc.postMessage(JSON.stringify({type:'restore_always',value:v})); }catch(e){} }
function postPip(v){ try{ window.ipc.postMessage(JSON.stringify({type:'pip_auto',value:v})); }catch(e){} }
function postAutofill(key,v){ try{ window.ipc.postMessage(JSON.stringify({type:key,value:v})); }catch(e){} }
function postAskDownloads(v){ try{ window.ipc.postMessage(JSON.stringify({type:'ask_downloads',value:v})); }catch(e){} }
function postHistoryRetention(v){ try{ window.ipc.postMessage(JSON.stringify({type:'history_retention',value:parseInt(v,10)||0})); }catch(e){} }
function postStartupMode(v){ try{ window.ipc.postMessage(JSON.stringify({type:'startup_mode',value:v})); }catch(e){} }
function startupUrlAdd(){ var el=document.getElementById('startupUrl'); var v=el?el.value:''; try{ window.ipc.postMessage(JSON.stringify({type:'startup_url_add',value:v})); }catch(e){} if(el)el.value=''; }
function startupUrlDel(u){ try{ window.ipc.postMessage(JSON.stringify({type:'startup_url_remove',value:u})); }catch(e){} }
window.__ferriteStartupUrls=function(list){
  var el=document.getElementById('startupList'); if(!el) return; el.textContent='';
  (list||[]).forEach(function(u){
    var d=document.createElement('div'); var c=document.createElement('code'); c.textContent=u; d.appendChild(c);
    var b=document.createElement('button'); b.textContent='Remove';
    b.onclick=(function(url){return function(){startupUrlDel(url);};})(u); d.appendChild(b); el.appendChild(d);
  });
  if(!(list||[]).length){ var e=document.createElement('div'); e.className='d'; e.textContent='No pages listed: "Open specific pages" falls back to a new tab.'; el.appendChild(e); }
};
function postSleepTimeout(v){ try{ window.ipc.postMessage(JSON.stringify({type:'sleep_timeout',value:parseInt(v,10)||0})); }catch(e){} }
function sleepExceptionAdd(){ var el=document.getElementById('sleepHost'); var v=el?el.value:''; try{ window.ipc.postMessage(JSON.stringify({type:'sleep_exception_add',value:v})); }catch(e){} if(el)el.value=''; }
function sleepExceptionDel(h){ try{ window.ipc.postMessage(JSON.stringify({type:'sleep_exception_remove',value:h})); }catch(e){} }
window.__ferriteSleepExceptions=function(list){
  var el=document.getElementById('sleepList'); if(!el) return; el.textContent='';
  (list||[]).forEach(function(h){
    var d=document.createElement('div'); var c=document.createElement('code'); c.textContent=h; d.appendChild(c);
    var b=document.createElement('button'); b.textContent='Remove';
    b.onclick=(function(host){return function(){sleepExceptionDel(host);};})(h); d.appendChild(b); el.appendChild(d);
  });
  if(!(list||[]).length){ var e=document.createElement('div'); e.className='d'; e.textContent='Nothing listed: every idle background tab may sleep.'; el.appendChild(e); }
};
function exclusionAdd(){ var v=document.getElementById('exOrigin').value; try{ window.ipc.postMessage(JSON.stringify({type:'exclusion_add',origin:v})); }catch(e){} document.getElementById('exOrigin').value=''; }
function exclusionDel(o){ try{ window.ipc.postMessage(JSON.stringify({type:'exclusion_remove',origin:o})); }catch(e){} }
function permGrantRevoke(o,k){ try{ window.ipc.postMessage(JSON.stringify({type:'perm_grant_revoke',origin:o,kind:k})); }catch(e){} }
function syncRefresh(){ try{ window.ipc.postMessage(JSON.stringify({type:'sync_devices'})); }catch(e){} }
function syncCodeShow(){ try{ window.ipc.postMessage(JSON.stringify({type:'sync_code_show'})); }catch(e){} }
function syncPairSubmit(){ var v=document.getElementById('pairCode').value; document.getElementById('pairCode').value=''; try{ window.ipc.postMessage(JSON.stringify({type:'sync_pair_submit',code:v})); }catch(e){} }
function syncRelaySave(){ var v=document.getElementById('relayUrls').value; try{ window.ipc.postMessage(JSON.stringify({type:'sync_relay_save',value:v})); }catch(e){} }
function syncRelayClear(){ var el=document.getElementById('relayUrls'); if(el)el.value=''; try{ window.ipc.postMessage(JSON.stringify({type:'sync_relay_clear'})); }catch(e){} }
function syncPairApprove(){ try{ window.ipc.postMessage(JSON.stringify({type:'sync_pair_approve'})); }catch(e){} }
function syncPairDeny(){ try{ window.ipc.postMessage(JSON.stringify({type:'sync_pair_deny'})); }catch(e){} }
function syncRevoke(id){ try{ window.ipc.postMessage(JSON.stringify({type:'sync_trust_revoke',id:id})); }catch(e){} }
function syncCategory(id,on){ try{ window.ipc.postMessage(JSON.stringify({type:'sync_category',id:id,on:on})); }catch(e){} }
function syncOpenTab(url){ try{ window.ipc.postMessage(JSON.stringify({type:'sync_open_tab',url:url})); }catch(e){} }
window.__ferriteSyncDevices=function(p){
  p=p||{};
  var self=document.getElementById('syncSelf'), list=document.getElementById('syncList'), pend=document.getElementById('syncPending');
  var status=document.getElementById('syncStatus'), cats=document.getElementById('syncCats');
  var peers=document.getElementById('syncPeers'), live=document.getElementById('syncLive');
  if(!self||!list||!pend||!status||!cats||!peers||!live) return;
  self.textContent=''; list.textContent=''; status.textContent=''; cats.textContent='';
  peers.textContent=''; live.textContent='';
  if(p.unavailable){ var u=document.createElement('div'); u.className='d'; u.textContent='Device trust needs the vault available.'; self.appendChild(u); pend.hidden=true; return; }
  var st=document.createElement('div'); st.className='d';
  st.textContent = p.transport==='lan'
    ? 'Transport: live on the local network.'
    : (p.transport==='off-incognito'
      ? 'Transport: off in Incognito (pairings here are session-only).'
      : 'Transport: off (unavailable on this network or still starting).');
  if(p.last_round&&p.last_round.at_ms){
    var lr=document.createElement('div'); lr.className='d';
    lr.textContent='Last round: '+p.last_round.peers_synced+' of '+p.last_round.peers_seen+' devices synced, '+p.last_round.actions_received+' changes.'
      +(p.last_round.deferred?(' '+p.last_round.deferred+' waiting for offline devices.'):'')
      +(p.last_round.catching_up?' Syncing…':'');
    st.appendChild(lr);
  }
  status.appendChild(st);
  var me=document.createElement('div'); var mc=document.createElement('code');
  mc.textContent='This device: '+(p.self_name||'This device')+' · '+p.self_id+' · fp '+p.self_fp;
  me.appendChild(mc); self.appendChild(me);
  (p.devices||[]).forEach(function(d){
    var row=document.createElement('div'); var c=document.createElement('code');
    c.textContent=d.name+' · '+d.id+' · fp '+d.fp+(d.revoked?' · REVOKED':'');
    row.appendChild(c);
    if(!d.revoked&&!d.is_self){
      var b=document.createElement('button'); b.textContent='Revoke';
      b.onclick=(function(id){return function(){syncRevoke(id);};})(d.id); row.appendChild(b);
    }
    list.appendChild(row);
  });
  if(!(p.devices||[]).length){ var e=document.createElement('div'); e.className='d'; e.textContent='No paired devices yet.'; list.appendChild(e); }
  (p.categories||[]).forEach(function(c){
    var lab=document.createElement('label'); lab.className='row';
    var box=document.createElement('input'); box.type='checkbox'; box.checked=!!c.on;
    box.onchange=(function(id){return function(){syncCategory(id,this.checked);};})(c.id);
    lab.appendChild(box); lab.appendChild(document.createTextNode(' '+c.label));
    cats.appendChild(lab);
  });
  var fh=document.createElement('div'); fh.className='d'; fh.textContent='On this network:'; peers.appendChild(fh);
  function ago(ms){
    if(!ms) return 'seen recently';
    var s=Math.max(0,Math.floor((Date.now()-ms)/1000));
    if(s<10) return 'seen just now';
    if(s<60) return 'seen '+s+'s ago';
    var m=Math.floor(s/60);
    if(m<60) return 'seen '+m+'m ago';
    return 'seen '+Math.floor(m/60)+'h ago';
  }
  (p.discovered||[]).forEach(function(d){
    var row=document.createElement('div'); var c=document.createElement('code');
    c.textContent=d.name+' · '+d.id+' · fp '+d.fp+' · '+ago(d.last_seen_ms);
    row.appendChild(c); peers.appendChild(row);
  });
  if(!(p.discovered||[]).length){ var n=document.createElement('div'); n.className='d'; n.textContent='No other Ferrite devices heard.'; peers.appendChild(n); }
  var lh=document.createElement('div'); lh.className='d'; lh.textContent='Live tabs from paired devices:'; live.appendChild(lh);
  (p.livetabs||[]).forEach(function(t){
    var row=document.createElement('div'); var c=document.createElement('code');
    c.textContent=t.url+' · from '+t.source;
    row.appendChild(c);
    var b=document.createElement('button'); b.textContent='Open';
    b.onclick=(function(url){return function(){syncOpenTab(url);};})(t.url); row.appendChild(b);
    live.appendChild(row);
  });
  if(!(p.livetabs||[]).length){ var m=document.createElement('div'); m.className='d'; m.textContent='Nothing live right now.'; live.appendChild(m); }
  if(p.pending){
    pend.hidden=false; pend.textContent='';
    var t=document.createElement('div');
    t.textContent='Pair '+p.pending.name+' · '+p.pending.id+' · fp '+p.pending.fp+'?';
    pend.appendChild(t);
    var ok=document.createElement('button'); ok.textContent='Approve'; ok.onclick=function(){syncPairApprove();}; pend.appendChild(ok);
    var no=document.createElement('button'); no.textContent='Deny'; no.onclick=function(){syncPairDeny();}; pend.appendChild(no);
  } else { pend.hidden=true; pend.textContent=''; }
  var relayUrls=document.getElementById('relayUrls'), relayState=document.getElementById('relayState');
  if(relayUrls&&relayState){
    var r=p.relay||{};
    var urls=r.urls||[];
    relayUrls.value=urls.join(', ');
    relayState.textContent = urls.length
      ? ('Relay: '+urls.join(', ')+(r.connected?' ; session live.':' ; not connected (next round retries).'))
      : 'Relay: none configured. LAN-only sync.';
  }
};
window.__ferriteSyncCode=function(p){
  var box=document.getElementById('syncCodeBox'), ta=document.getElementById('syncCode');
  if(!box||!ta) return;
  // QR art is server-rendered SVG (rects only, no scripts): safe for
  // innerHTML, unlike any page-controlled string on this page.
  var q=document.getElementById('syncQr');
  if(window.__ferriteSyncCodeTimer){ clearInterval(window.__ferriteSyncCodeTimer); window.__ferriteSyncCodeTimer=null; }
  var left=document.getElementById('syncCodeLeft');
  function fmtLeft(ms){
    if(ms<=0) return 'expired';
    var s=Math.ceil(ms/1000);
    return s>=90 ? ('expires in '+Math.floor(s/60)+' min') : ('expires in '+s+' s');
  }
  if(p&&p.code){
    ta.value=p.code; box.hidden=false; if(q){ q.innerHTML=p.qr||''; }
    if(p.expires_at_ms&&left){
      function tick(){
        var rest=p.expires_at_ms-Date.now();
        if(rest<=0){ ta.value=''; if(q) q.textContent=''; box.hidden=true; left.textContent=''; clearInterval(window.__ferriteSyncCodeTimer); window.__ferriteSyncCodeTimer=null; return; }
        left.textContent=fmtLeft(rest);
      }
      tick();
      window.__ferriteSyncCodeTimer=setInterval(tick,15000);
    } else if(left) left.textContent='';
  }
  else { ta.value=''; if(q) q.textContent=''; box.hidden=true; if(left) left.textContent=''; }
};
window.__ferritePermGrants=function(list){
  var el=document.getElementById('permGrants'); el.textContent='';
  (list||[]).forEach(function(g){
    var d=document.createElement('div'); var c=document.createElement('code'); c.textContent=g.label+', '+g.origin; d.appendChild(c);
    if(g.source && g.source!=='user'){
      var s=document.createElement('span'); s.className='d';
      s.textContent=g.expires_at_ms?(' · auto-blocked until '+new Date(g.expires_at_ms).toLocaleDateString()):' · auto-blocked';
      d.appendChild(s);
    }
    var b=document.createElement('button'); b.textContent='Revoke';
    b.onclick=(function(origin,kind){return function(){permGrantRevoke(origin,kind);};})(g.origin,g.kind); d.appendChild(b); el.appendChild(d);
  });
  if(!(list||[]).length){ var e=document.createElement('div'); e.className='d'; e.textContent='No permanent grants.'; el.appendChild(e); }
};
window.__ferriteExclusions=function(list){
  var el=document.getElementById('exList'); el.textContent='';
  (list||[]).forEach(function(o){
    var d=document.createElement('div'); var c=document.createElement('code'); c.textContent=o; d.appendChild(c);
    var b=document.createElement('button'); b.textContent='Remove';
    b.onclick=(function(origin){return function(){exclusionDel(origin);};})(o); d.appendChild(b); el.appendChild(d);
  });
};
function saveHome(){ try{ window.ipc.postMessage(JSON.stringify({type:'select_home',url:document.getElementById('homeUrl').value})); }catch(e){} }
function postRecoveryShow(){ try{ window.ipc.postMessage(JSON.stringify({type:'recovery_show'})); }catch(e){} }
function postRecoveryDisable(){ try{ window.ipc.postMessage(JSON.stringify({type:'recovery_disable'})); }catch(e){} }
function saveRealtimeKey(){ var el=document.getElementById('realtimeKey'); try{ window.ipc.postMessage(JSON.stringify({type:'realtime_key',value:el?el.value:''})); if(el)el.value=''; }catch(e){} }
function clearRealtimeKey(){ try{ window.ipc.postMessage(JSON.stringify({type:'realtime_key',value:''})); }catch(e){} }
function saveDoh(){ var el=document.getElementById('dohEndpoint'); try{ window.ipc.postMessage(JSON.stringify({type:'doh_endpoint',value:el?el.value:''})); if(el)el.value=''; }catch(e){} }
function clearDoh(){ try{ window.ipc.postMessage(JSON.stringify({type:'doh_endpoint',value:''})); }catch(e){} }
window.__ferriteRecovery=function(d){
  var box=document.getElementById('recBox'), ph=document.getElementById('recPhrase');
  if(!box||!ph) return;
  if(d&&d.phrase){ ph.textContent=d.phrase; box.hidden=false; }
  else { ph.textContent=''; box.hidden=true; }
};
function saveSpellLangs(){ try{ window.ipc.postMessage(JSON.stringify({type:'spellcheck_langs',value:document.getElementById('spellLangs').value})); }catch(e){} }
function saveTranslateTarget(){ try{ window.ipc.postMessage(JSON.stringify({type:'translate_target',value:document.getElementById('translateTarget').value})); }catch(e){} }
function clearData(){ try{ window.ipc.postMessage(JSON.stringify({type:'clear_data',range:document.getElementById('clearRange').value,history:document.getElementById('clearHistory').checked,downloads:document.getElementById('clearDownloads').checked,cache:document.getElementById('clearCache').checked,cookies:document.getElementById('clearCookies').checked})); }catch(e){} }
function cookiesQuery(){ var v=document.getElementById('ckOrigin').value; try{ window.ipc.postMessage(JSON.stringify({type:'cookies_query',origin:v})); }catch(e){} }
function cookiesClearSite(){ var v=document.getElementById('ckOrigin').value; try{ window.ipc.postMessage(JSON.stringify({type:'cookies_clear_site',origin:v})); }catch(e){} }
window.__ferriteCookies=function(p){
  var el=document.getElementById('ckList'); el.textContent='';
  var list=(p&&p.cookies)||[];
  if(!p||!p.origin){ var e=document.createElement('div'); e.className='d'; e.textContent='Type an origin above (bare host is fine).'; el.appendChild(e); return; }
  if(!list.length){ var n=document.createElement('div'); n.className='d'; n.textContent='No cookies stored for '+p.origin+'.'; el.appendChild(n); return; }
  list.forEach(function(c){
    var d=document.createElement('div'); var cd=document.createElement('code');
    cd.textContent=c.name+', '+c.domain+c.path+' · expires '+c.expiry+(c.third?' · third-party':'');
    d.appendChild(cd); el.appendChild(d);
  });
};
window.__ferriteCookiesCleared=function(p){
  var el=document.getElementById('ckOut');
  if(el) el.textContent='Cleared '+(p?p.cleared:0)+' cookies for '+(p?p.origin:'')+'.';
};
function customAdd(){ try{ window.ipc.postMessage(JSON.stringify({type:'custom_add',name:document.getElementById('cName').value,template:document.getElementById('cTpl').value})); }catch(e){} }
function customDel(id){ try{ window.ipc.postMessage(JSON.stringify({type:'custom_delete',id:id})); }catch(e){} }
function importBookmarks(){ try{ window.ipc.postMessage(JSON.stringify({type:'import',kind:'bookmarks'})); }catch(e){} }
function importHistory(){ try{ window.ipc.postMessage(JSON.stringify({type:'import',kind:'history'})); }catch(e){} }
function exportData(){ try{ window.ipc.postMessage(JSON.stringify({type:'import',kind:'export'})); }catch(e){} }
function exportHistory(){ try{ window.ipc.postMessage(JSON.stringify({type:'import',kind:'export_history'})); }catch(e){} }
function exportSettings(){ try{ window.ipc.postMessage(JSON.stringify({type:'export_settings'})); }catch(e){} }
function importSettings(){ try{ window.ipc.postMessage(JSON.stringify({type:'import_settings'})); }catch(e){} }
function exportAllArm(){
  var b=document.getElementById('exportAllBtn');
  if(!b.armed){b.armed=true;b.textContent='Confirm: passwords leave as plaintext?';return;}
  b.armed=false;b.textContent='Export everything (.tar)';
  try{ window.ipc.postMessage(JSON.stringify({type:'export_all'})); }catch(e){}
}
function backupExport(){
  var p=document.getElementById('backupPass').value;
  document.getElementById('backupPass').value='';
  if(p.length<8){document.getElementById('exportOut').textContent='Passphrase needs 8+ characters.';return;}
  try{ window.ipc.postMessage(JSON.stringify({type:'backup_export',passphrase:p})); }catch(e){}
}
function backupImport(){ try{ window.ipc.postMessage(JSON.stringify({type:'backup_import'})); }catch(e){} }
window.__ferriteBackupPrompt=function(){
  document.getElementById('exportOut').textContent='Backup picked. Enter its passphrase above, then Restore.';
  document.getElementById('backupPass').focus();
};
function backupRestore(){
  var p=document.getElementById('backupPass').value;
  document.getElementById('backupPass').value='';
  try{ window.ipc.postMessage(JSON.stringify({type:'backup_restore',passphrase:p})); }catch(e){}
}
function resetSettings(){ try{ window.ipc.postMessage(JSON.stringify({type:'reset_settings'})); }catch(e){} }
try{ window.ipc.postMessage(JSON.stringify({type:'settings_query'})); }catch(e){}
try{ window.ipc.postMessage(JSON.stringify({type:'exclusions_query'})); }catch(e){}
try{ window.ipc.postMessage(JSON.stringify({type:'perm_grants_query'})); }catch(e){}
try{ window.ipc.postMessage(JSON.stringify({type:'sync_devices'})); }catch(e){}
window.__ferriteSetSettings = function(s){
  try{
    var p=s.privacy||s;
    document.getElementById('adblock').checked = !!p.adblock_enabled;
    document.getElementById('gpc').checked = !!p.gpc_enabled;
    document.getElementById('realtime').checked = !!p.realtime_lookup_enabled;
    var rk=document.getElementById('realtimeKeyState');
    if(rk) rk.textContent = p.realtime_api_key_set ? 'API key is set.' : 'No API key set. Lookup is inert until you save one.';
    var dohIn=document.getElementById('dohEndpoint');
    var ds=document.getElementById('dohState');
    if(p.doh_endpoint){ if(dohIn) dohIn.value=p.doh_endpoint; if(ds) ds.textContent='DoH on: '+p.doh_endpoint; }
    else { if(dohIn) dohIn.value=''; if(ds) ds.textContent='DoH off. OS DNS in the clear.'; }
    document.getElementById('https_up').checked = !!p.https_upgrade_enabled;
    document.getElementById('suggestions').checked = s.suggestions !== false;
    document.getElementById('suggestions_remote').checked = !!s.suggestions_remote;
    document.getElementById('restoreAlways').checked = !!s.restore_always;
    document.getElementById('tabsVertical').checked = !!s.tabs_vertical;
    document.getElementById('pipAuto').checked = !!s.pip_auto;
    document.getElementById('autofillAddress').checked = s.autofill_address !== false;
    document.getElementById('notifGlobal').checked = s.notifications_global !== false;
    document.getElementById('autofillPayment').checked = s.autofill_payment !== false;
    document.getElementById('autofillForm').checked = s.autofill_form !== false;
    document.getElementById('askDownloads').checked = !!s.ask_downloads;
    var retentionSel=document.getElementById('historyRetention');
    if(retentionSel){
      var rv=(typeof s.history_retention_days==='number')?s.history_retention_days:90;
      var allowed=['30','90','180','365','0'];
      retentionSel.value=allowed.indexOf(String(rv))>=0?String(rv):'90';
    }
    var startupSel=document.getElementById('startupMode');
    if(startupSel) startupSel.value=(s.startup_mode==='newtab'||s.startup_mode==='urls')?s.startup_mode:'restore';
    if(window.__ferriteStartupUrls) window.__ferriteStartupUrls(s.startup_urls);
    var pickerEl=document.getElementById('profilePicker');
    if(pickerEl) pickerEl.checked=s.profile_picker!==false;
    var sleepSel=document.getElementById('sleepTimeout');
    if(sleepSel) sleepSel.value=String((typeof s.sleep_timeout_min==='number')?s.sleep_timeout_min:30);
    if(window.__ferriteSleepExceptions) window.__ferriteSleepExceptions(s.sleep_exceptions);
    document.getElementById('spellcheck').checked = p.spellcheck_enabled !== false;
    if(p.spellcheck_langs) document.getElementById('spellLangs').value = p.spellcheck_langs.join(', ');
    document.getElementById('translate').checked = s.translate_enabled !== false;
    if(s.translate_target) document.getElementById('translateTarget').value = s.translate_target;
    if(s.home_url) document.getElementById('homeUrl').value = s.home_url;
    var eng=document.getElementById('engine'); eng.textContent='';
    (s.engines||[]).forEach(function(e){ var o=document.createElement('option'); o.value=e.id; o.textContent=e.name; if(e.current)o.selected=true; eng.appendChild(o); });
    var engP=document.getElementById('enginePrivate'); if(engP){ engP.textContent='';
      var f=document.createElement('option'); f.value=''; f.textContent='Follow regular default'; if(!s.private_engine)f.selected=true; engP.appendChild(f);
      (s.engines||[]).forEach(function(e){ var o=document.createElement('option'); o.value=e.id; o.textContent=e.name; if(s.private_engine&&s.private_engine===e.id)o.selected=true; engP.appendChild(o); });
    }
    var pk=document.getElementById('pack'); pk.textContent='';
    (s.packs||[]).forEach(function(e){ var o=document.createElement('option'); o.value=e.id; o.textContent=e.name; if(e.current)o.selected=true; pk.appendChild(o); });
    var cl=document.getElementById('customList'); cl.textContent='';
    (s.customs||[]).forEach(function(e){ var d=document.createElement('div'); var c=document.createElement('code'); c.textContent=e.name+', '+e.template+' '; d.appendChild(c); var b=document.createElement('button'); b.textContent='Remove'; b.onclick=(function(id){return function(){customDel(id);};})(e.id); d.appendChild(b); cl.appendChild(d); });
  }catch(e){}
};
window.__ferriteCleared = function(msg){ document.getElementById('clearOut').textContent = msg; };
window.__ferriteAdvanced = function(msg){ document.getElementById('advOut').textContent = msg; };
var engSel=document.getElementById('engine');
engSel.addEventListener('change',function(){ try{ window.ipc.postMessage(JSON.stringify({type:'select_engine',id:engSel.value})); }catch(e){} });
var engPrivSel=document.getElementById('enginePrivate');
if(engPrivSel){ engPrivSel.addEventListener('change',function(){ try{ window.ipc.postMessage(JSON.stringify({type:'select_private_engine',id:engPrivSel.value})); }catch(e){} }); }
var pkSel=document.getElementById('pack');
pkSel.addEventListener('change',function(){ try{ window.ipc.postMessage(JSON.stringify({type:'select_pack',id:pkSel.value})); }catch(e){} });
var sq=document.getElementById('setsearch');
sq.addEventListener('input',function(){
  var needle=sq.value.trim().toLowerCase();
  document.querySelectorAll('#setbody .set').forEach(function(el){
    var hay=(el.textContent+' '+(el.getAttribute('data-keys')||'')).toLowerCase();
    el.style.display=(!needle||hay.indexOf(needle)>=0)?'':'none';
  });
  document.querySelectorAll('#setbody section').forEach(function(sec){
    var any=false;
    sec.querySelectorAll('.set').forEach(function(el){ if(el.style.display!=='none')any=true; });
    sec.style.display=any?'':'none';
  });
});
// Short deep links (#privacy) resolve to section anchors (#cat-privacy);
// unknown hashes are left for the engine (native in-page behavior).
// ponytail: no #site/[origin] alias, per-site state has no backend yet.
// It earns the alias when the broker lands.
(function(){
  var SECTIONS={privacy:'cat-privacy',search:'cat-search',appearance:'cat-appearance',downloads:'cat-downloads',startup:'cat-startup',performance:'cat-performance',advanced:'cat-advanced',about:'cat-about',sync:'cat-sync'};
  var h=(location.hash||'').replace(/^#/,'');
  if(h&&!document.getElementById('cat-'+h)&&SECTIONS[h]){ location.replace('#'+SECTIONS[h]); }
})();
// Search resolves to fragments: Enter jumps to the first match's section
// through a real fragment navigation (history + back button included).
sq.addEventListener('keydown',function(e){
  if(e.key!=='Enter') return;
  var first=null;
  document.querySelectorAll('#setbody .set').forEach(function(el){ if(!first&&el.style.display!=='none')first=el; });
  if(!first) return;
  sq.value='';
  document.querySelectorAll('#setbody .set').forEach(function(el){ el.style.display=''; });
  document.querySelectorAll('#setbody section').forEach(function(sec){ sec.style.display=''; });
  var sec=first.closest('section');
  if(sec&&sec.id) location.hash=sec.id;
});
</script>
</body></html>
"##;

/// Shell-driven list page shell: Rust pushes rows via the named hook;
/// all row text is set through `textContent`, never `innerHTML`, so vault
/// titles/URLs cannot become markup. Search box posts `{type, action,
/// query}`; rows post `{type, action:'open', url}` or `{'delete', id}`.
macro_rules! list_page {
    ($title:expr, $hint:expr, $hook:expr, $kind:expr) => {
        concat!(
            "<!DOCTYPE html><!-- ferrite://",
            $kind,
            " --><html><head><meta charset=\"utf-8\"><title>",
            $title,
            " - Ferrite</title><style>/*__FERRITE_THEME__*/",
            "body{background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui);margin:0;padding:32px;}h1{font-size:20px;margin:0 0 16px;}",
            "#q{background:var(--f-surface-1);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-card-radius,8px);padding:8px 12px;width:min(480px,90%);font-size:14px;margin-bottom:16px;}",
            ".row{display:flex;gap:12px;align-items:center;padding:8px 4px;border-bottom:1px solid var(--f-border);cursor:pointer;}",
            ".row:hover{background:var(--f-surface-1);}.row .t{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}",
            ".row .u{color:var(--f-muted);font-size:12px;max-width:40%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}",
            ".row button{background:none;border:1px solid var(--f-border);color:var(--f-muted);border-radius:var(--pk-ctl-radius,6px);cursor:pointer;}",
            "@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important;}}</style></head>",
            "<body><h1>",
            $title,
            "</h1><input id=\"q\" placeholder=\"",
            $hint,
            "\" autocomplete=\"off\" spellcheck=\"false\"><div id=\"list\"></div>",
            "<script>function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}",
            "var KIND='",
            $kind,
            "';window['",
            $hook,
            "']=function(items){var l=document.getElementById('list');l.textContent='';",
            "if(!items||!items.length){var e=document.createElement('div');",
            "e.style.cssText='color:var(--f-muted);padding:16px 4px;';",
            "e.textContent=KIND==='downloads'?'No downloads yet':(KIND==='history'?'No history yet \\u2014 pages you visit appear here':'No bookmarks yet \\u2014 star a page to pin it here');",
            "l.appendChild(e);return;}",
            "(items||[]).forEach(function(it){var r=document.createElement('div');r.className='row';",
            "var t=document.createElement('span');t.className='t';t.textContent=it.title||it.url||it.filename||'';r.appendChild(t);",
            "var u=document.createElement('span');u.className='u';u.textContent=it.url||it.state||'';r.appendChild(u);",
            "if(it.id&&KIND==='bookmarks'){var b=document.createElement('button');b.textContent='Delete';b.onclick=function(e){e.stopPropagation();post({type:KIND+'_delete',id:it.id});};r.appendChild(b);}",
            "if(it.id&&KIND==='history'){var d=document.createElement('button');d.textContent='Delete';d.onclick=function(e){e.stopPropagation();post({type:'history_delete',id:it.id,query:q.value});};r.appendChild(d);",
            "if(it.url&&KIND==='history'){var cb=document.createElement('button');cb.textContent='Copy link';cb.onclick=function(e){e.stopPropagation();post({type:'dl_copy',text:it.url});};r.appendChild(cb);}",            "try{var hu=new URL(it.url||'');if(KIND==='history'&&hu.hostname){var sb=document.createElement('button');sb.textContent='Delete site';sb.onclick=function(e){e.stopPropagation();post({type:'history_delete_site',host:hu.hostname,query:q.value});};r.appendChild(sb);}}catch(_){}",
            "r.onclick=function(){var url=it.url;if(url)post({type:'navigate',url:url});};l.appendChild(r);});};",
            "var q=document.getElementById('q');",
            // Query-param contract (?q=, ?site=): deep links preset the
            // filter; typing only replaces state (no history spam), Enter
            // commits a real navigation (back returns to the unfiltered list).
            "try{var sp=new URLSearchParams(location.search);var preset=sp.get('q')||sp.get('site')||'';if(preset)q.value=preset;}catch(_){}",
            "q.addEventListener('input',function(){post({type:KIND+'_query',query:q.value});",
            "if(KIND==='history'){try{history.replaceState(null,'',q.value?('?q='+encodeURIComponent(q.value)):location.pathname);}catch(_){}}",
            "});",
            "q.addEventListener('keydown',function(e){if(e.key==='Escape')post({type:'dismiss'});",
            "if(KIND==='history'&&e.key==='Enter')post({type:'history_commit',query:q.value});});",
            "post({type:KIND+'_query',query:q.value});</script></body></html>"
        )
    };
}

pub const HISTORY_HTML: &str =
    list_page!("History", "Filter history", "__ferriteHistory", "history");
/// Site details: everything Ferrite holds for one origin (history,
/// logins, permissions, zoom, cookies, session blocks) plus a single
/// forget action. Rendered from the `__ferriteSite` hook (validated
/// origin); the URL param only tells the shell what was asked for.
pub const SITE_HTML: &str = r#"<!DOCTYPE html><!-- ferrite://site -->
<html><head><meta charset="utf-8"><title>Site Details - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
#wrap{padding:24px 32px;max-width:720px}
h1{font-size:20px;margin:0 0 4px;overflow-wrap:anywhere}
.sub{color:var(--f-muted);font-size:13px;margin:0 0 16px}
table{border-collapse:collapse;font-size:13px;margin:8px 0 0;width:100%}
td{padding:6px 16px 6px 0;vertical-align:top;border-bottom:1px solid var(--f-border)}
td.k{color:var(--f-muted);white-space:nowrap;width:150px}
ul.flat{list-style:none;margin:4px 0 0;padding:0;font-size:13px}
ul.flat li{padding:3px 0}
button{background:none;border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-ctl-radius,6px);padding:7px 14px;cursor:pointer;font-size:13px;font-family:inherit;margin-top:16px}
button:hover{border-color:var(--f-accent)}
button.danger:hover{color:var(--f-danger);border-color:var(--f-danger)}
.small{color:var(--f-faint);font-size:12px;margin-top:8px}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}</style></head>
<body><div id="wrap">
<h1 id="host">Site</h1>
<p class="sub" id="conn"></p>
<table id="rows" aria-label="Stored site data"></table>
<p><button id="forget" class="danger">Forget this site…</button></p>
<p class="small" id="out" aria-live="polite"></p>
<p class="small">Deletes history, saved logins, permission grants, zoom level, cookies, and remembered form values for this origin. Bookmarks and download records stay (they are yours to manage, not site residue).</p>
</div>
<script>
(function(){
  function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}
  function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');}
  var origin='';
  function row(k,v){return '<tr><td class="k">'+esc(k)+'</td><td>'+v+'</td></tr>';}
  function li(items){return '<ul class="flat">'+items.map(function(x){return '<li>'+esc(x)+'</li>';}).join('')+'</ul>';}
  function fmtDate(ms){if(!ms)return'never';try{return new Date(ms).toLocaleString();}catch(e){return'';}}
  window.__ferriteSite=function(d){
    d=d||{};origin=d.origin||'';
    document.getElementById('host').textContent=origin||'Site';
    document.getElementById('conn').textContent=d.secure
      ?'Secure connection (HTTPS). Details under the shield icon.'
      :'Plain HTTP connection. Anything sent is readable in transit.';
    var h='';
    h+=row('History',(d.visits||0)+' visits, last '+fmtDate(d.last_visit));
    h+=row('Saved logins',d.logins&&d.logins.length?li(d.logins):'none');
    h+=row('Permissions',d.perms&&d.perms.length?li(d.perms):'none');
    h+=row('Zoom',(d.zoom&&d.zoom!==100)?d.zoom+'%':'default (100%)');
    h+=row('Cookies',(d.cookies||0)+' in the profile jar');
    h+=row('Form entries',(d.forms||0)+' remembered values');
    h+=row('Blocked this session',String(d.blocked||0)+' requests');
    document.getElementById('rows').innerHTML=h;
  };
  document.getElementById('forget').addEventListener('click',function(){
    if(!origin)return;
    if(confirm('Forget '+origin+'? History, logins, permissions, zoom, cookies, and form entries for this origin will be deleted.')){
      post({type:'site_forget',origin:origin});
    }
  });
  try{
    var q=new URLSearchParams(location.search).get('origin')||'';
    if(q)post({type:'site_query',origin:q});
  }catch(e){}
})();
</script></body></html>
"#;
/// PDF viewer: page images plus exact overlays (selectable text lines,
/// clickable links, search highlights), all positioned in page
/// fractions so zoom is pure CSS. Served with `img-src data:` (the one
/// CSP delta): bitmaps arrive as data URLs from the shell, which
/// PNG-encodes the isolated renderer's RGBA. All strings from the
/// document are untrusted: the script builds DOM with `textContent`,
/// never `innerHTML`.
pub const PDF_HTML: &str = r##"<!DOCTYPE html><!-- ferrite://pdf/ -->
<html><head><meta charset="utf-8"><title>PDF - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;height:100%;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui);font-size:13px;overflow:hidden}
#bar{display:flex;align-items:center;gap:6px;height:44px;padding:0 10px;background:var(--f-surface-1);border-bottom:1px solid var(--f-border)}
#bar .t{font-weight:700;font-size:13px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:280px;margin:0 6px 0 2px}
#bar button,#bar select{background:var(--f-surface-2);border:1px solid var(--f-border);color:var(--f-fg);border-radius:6px;padding:6px 10px;font-size:12.5px;font-family:inherit;cursor:pointer;flex:none}
#bar button:hover:not(:disabled){border-color:var(--f-accent)}
#bar button:disabled{opacity:.4;cursor:default}
#bar select{padding:5px 6px}
#pagebox{display:flex;align-items:center;gap:4px;flex:none}
#pagein{width:52px;background:var(--f-surface-2);border:1px solid var(--f-border);color:var(--f-fg);border-radius:6px;padding:5px 8px;font-size:12.5px;font-family:inherit;text-align:right}
#pagecount{color:var(--f-muted);font-size:12.5px;white-space:nowrap}
#status{margin-left:auto;color:var(--f-muted);font-size:12px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:260px}
#wrap{display:flex;height:calc(100% - 45px)}
#side{width:264px;flex:none;background:var(--f-surface-1);border-right:1px solid var(--f-border);display:flex;flex-direction:column;overflow:hidden}
#side.hidden{display:none}
#sidetabs{display:flex;border-bottom:1px solid var(--f-border);flex:none}
#sidetabs button{flex:1;background:none;border:none;border-bottom:2px solid transparent;color:var(--f-muted);font-size:12px;font-family:inherit;padding:9px 0;cursor:pointer}
#sidetabs button.on{color:var(--f-fg);border-bottom-color:var(--f-accent)}
#sidebody{flex:1;overflow-y:auto;padding:8px}
.pane{display:none}
.pane.on{display:block}
.thumb{display:block;width:100%;margin:0 0 10px;background:none;border:1px solid var(--f-border);border-radius:6px;padding:6px;cursor:pointer;color:var(--f-fg);font-family:inherit}
.thumb:hover{border-color:var(--f-accent)}
.thumb.cur{border-color:var(--f-accent);box-shadow:0 0 0 1px var(--f-accent)}
.thumb img{display:block;width:100%;height:auto;background:#fff;border-radius:3px}
.thumb span{display:block;font-size:11px;color:var(--f-muted);margin-top:4px;text-align:center}
.oline{display:block;width:100%;text-align:left;background:none;border:none;color:var(--f-fg);font-size:12.5px;font-family:inherit;padding:6px 8px;border-radius:6px;cursor:pointer;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.oline:hover{background:var(--f-surface-2)}
.srow{padding:8px;border:1px solid var(--f-border);border-radius:6px;margin-bottom:8px}
.srow .k{font-size:11px;color:var(--f-muted);margin-bottom:4px}
.srow label{display:block;font-size:12px;margin:4px 0}
.srow input[type=text]{width:100%;box-sizing:border-box;background:var(--f-surface-2);border:1px solid var(--f-border);color:var(--f-fg);border-radius:6px;padding:6px 8px;font-size:12.5px;font-family:inherit}
.srow input[type=checkbox]{margin-right:6px}
.srow select{width:100%;background:var(--f-surface-2);border:1px solid var(--f-border);color:var(--f-fg);border-radius:6px;padding:6px;font-size:12.5px;font-family:inherit}
.srow button{background:var(--f-surface-2);border:1px solid var(--f-border);color:var(--f-fg);border-radius:6px;padding:5px 10px;font-size:12px;font-family:inherit;cursor:pointer;margin-top:6px}
.srow button:hover:not(:disabled){border-color:var(--f-accent)}
.srow button:disabled{opacity:.4;cursor:default}
.srow .ro{font-size:12px;color:var(--f-muted)}
#searchbox{display:flex;gap:6px;margin-bottom:8px}
#searchq{flex:1;min-width:0;background:var(--f-surface-2);border:1px solid var(--f-border);color:var(--f-fg);border-radius:6px;padding:6px 8px;font-size:12.5px;font-family:inherit}
.sres{display:block;width:100%;text-align:left;background:none;border:1px solid var(--f-border);color:var(--f-fg);border-radius:6px;padding:6px 8px;font-size:12px;font-family:inherit;cursor:pointer;margin-bottom:6px}
.sres:hover{border-color:var(--f-accent)}
#main{flex:1;overflow:auto;display:flex;justify-content:center;align-items:flex-start;padding:24px;min-width:0}
#page{position:relative;flex:none;background:#fff;box-shadow:0 8px 40px rgba(0,0,0,.5)}
#page img{display:block;width:100%;height:auto}
.ov{position:absolute;inset:0;pointer-events:none}
#textlayer .ln{position:absolute;color:transparent;white-space:pre;overflow:hidden;user-select:text;pointer-events:auto;line-height:1}
#textlayer .ln::selection{background:rgba(79,70,229,.35)}
#linklayer a{position:absolute;display:block;pointer-events:auto;border-radius:2px}
#linklayer a:hover{background:rgba(79,70,229,.18);outline:1px solid rgba(79,70,229,.5)}
#matchlayer .m{position:absolute;background:rgba(255,180,0,.42);outline:1px solid rgba(255,140,0,.8);border-radius:1px}
#banner{display:none;margin:0 0 12px;padding:10px 12px;background:var(--f-surface-2);border:1px solid var(--f-warning);border-radius:8px;font-size:12.5px;color:var(--f-fg)}
#banner.on{display:block}
#errbox{display:none;text-align:center;color:var(--f-muted);padding:60px 20px;font-size:14px;max-width:460px}
#errbox.on{display:block}
#errbox button{margin-top:16px;background:var(--f-surface-2);border:1px solid var(--f-border);color:var(--f-fg);border-radius:6px;padding:7px 14px;font-size:13px;font-family:inherit;cursor:pointer}
#modal{position:fixed;inset:0;display:none;align-items:center;justify-content:center;background:rgba(0,0,0,.55);z-index:10}
#modal.on{display:flex}
#mbox{background:var(--f-surface-1);border:1px solid var(--f-border);border-radius:10px;padding:24px;width:min(360px,90%);box-shadow:0 16px 60px rgba(0,0,0,.5)}
#mbox h2{font-size:15px;margin:0 0 6px}
#mbox p{font-size:12.5px;color:var(--f-muted);margin:0 0 14px}
#mbox input{width:100%;box-sizing:border-box;background:var(--f-surface-2);border:1px solid var(--f-border);color:var(--f-fg);border-radius:6px;padding:8px 10px;font-size:13px;font-family:inherit;margin-bottom:12px}
#mbox .row{display:flex;gap:8px;justify-content:flex-end}
#mbox button{background:var(--f-surface-2);border:1px solid var(--f-border);color:var(--f-fg);border-radius:6px;padding:7px 14px;font-size:13px;font-family:inherit;cursor:pointer}
#mbox button.primary{background:var(--f-accent);border-color:var(--f-accent);color:var(--f-bg);font-weight:700}
#pwerr{display:none;font-size:12px;color:var(--f-danger);margin:-6px 0 10px}
#pwerr.on{display:block}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}
</style></head>
<body>
<div id="bar" role="toolbar" aria-label="PDF viewer">
<button id="bSide" title="Toggle sidebar">|||</button>
<span class="t" id="doctitle">PDF</span>
<div id="pagebox">
<button id="bPrev" title="Previous page">&lt;</button>
<input id="pagein" value="1" inputmode="numeric" aria-label="Page number">
<span id="pagecount">/ 0</span>
<button id="bNext" title="Next page">&gt;</button>
</div>
<button id="bZout" title="Zoom out">-</button>
<select id="zoom" aria-label="Zoom">
<option value="fit-w">Fit width</option>
<option value="fit-h">Fit height</option>
<option value="50">50%</option>
<option value="75">75%</option>
<option value="100">100%</option>
<option value="125">125%</option>
<option value="150">150%</option>
<option value="200">200%</option>
<option value="300">300%</option>
</select>
<button id="bZin" title="Zoom in">+</button>
<button id="bSearch" title="Find in document (Ctrl+F)">Find</button>
<button id="bPrint" title="Print (Ctrl+P)">Print</button>
<button id="bSave" title="Save a copy" disabled>Save</button>
<span id="status" role="status" aria-live="polite"></span>
</div>
<div id="wrap">
<div id="side">
<div id="sidetabs" role="tablist">
<button data-pane="thumbs" class="on" role="tab">Pages</button>
<button data-pane="outline" role="tab">Outline</button>
<button data-pane="search" role="tab">Search</button>
<button data-pane="fields" role="tab">Fields</button>
</div>
<div id="sidebody">
<div class="pane on" id="pane-thumbs"></div>
<div class="pane" id="pane-outline"></div>
<div class="pane" id="pane-search">
<div id="searchbox">
<input id="searchq" placeholder="Find in document" aria-label="Find in document">
<button id="bGo">Go</button>
</div>
<label style="font-size:12px;color:var(--f-muted)"><input type="checkbox" id="casebox"> Match case</label>
<div id="sres" style="margin-top:8px"></div>
</div>
<div class="pane" id="pane-fields"></div>
</div>
</div>
<div id="main"><div style="width:100%">
<div id="banner" role="note"></div>
<div id="page" hidden>
<img id="pimg" alt="PDF page">
<div class="ov" id="textlayer"></div>
<div class="ov" id="linklayer"></div>
<div class="ov" id="matchlayer"></div>
</div>
<div id="errbox"><div id="errmsg"></div><div><button id="bRetry">Retry</button></div></div>
</div></div>
</div>
<div id="modal" role="dialog" aria-label="Password required">
<div id="mbox">
<h2>Password required</h2>
<p>This document is encrypted. The password is used once to open it and never stored.</p>
<input id="pwin" type="password" autocomplete="off" placeholder="Document password" aria-label="Document password">
<div id="pwerr">Wrong password. Try again.</div>
<div class="row"><button id="bCancel">Cancel</button><button id="bUnlock" class="primary">Unlock</button></div>
</div>
</div>
<script>
(function(){
'use strict';
function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}
function el(tag,cls,text){var e=document.createElement(tag);if(cls)e.className=cls;if(text!=null)e.textContent=text;return e;}
function pct(f){return (f*100).toFixed(3)+'%';}
var docId=null;
var S={pages:0,page:0,locked:false,engine:true,perms:{print:true,copy:true,fill:true},outline:[],fields:[],dirty:false,mode:'fit-w',custom:100,baseW:0,baseH:0,reqSeq:0,lastQ:'',thumbs:{},searchQ:''};
var $=function(id){return document.getElementById(id);};
var main=$('main'),pageEl=$('page'),pimg=$('pimg'),textL=$('textlayer'),linkL=$('linklayer'),matchL=$('matchlayer');
function status(t){$('status').textContent=t||'';}
function fail(t){status(t);$('errmsg').textContent=t;$('errbox').classList.add('on');$('page').hidden=true;}
function clearFail(){$('errbox').classList.remove('on');$('page').hidden=false;}
function curScaleMilli(){
  var w=S.baseW,h=S.baseH;
  if(!w||!h)return 1000;
  var avail=main.clientWidth-48;
  if(avail<50)avail=50;
  if(S.mode==='fit-w')return Math.max(50,Math.min(8000,Math.round(avail/w*1000)));
  if(S.mode==='fit-h'){var ah=main.clientHeight-48;if(ah<50)ah=50;return Math.max(50,Math.min(8000,Math.round(ah/h*1000)));}
  return Math.max(50,Math.min(8000,Math.round(S.custom*10)));
}
var renderTimer=null;
function requestPage(page,q){
  if(S.pages<1)return;
  page=Math.max(0,Math.min(S.pages-1,page|0));
  S.page=page;
  S.lastQ=q||'';
  $('pagein').value=String(page+1);
  markThumb();
  S.reqSeq++;
  var scale=curScaleMilli();
  S.pendingScale=scale;
  post({type:'pdf_page',doc:docId,page:page,scale:scale,q:q||null});
  status('Rendering page '+(page+1)+'...');
}
function scheduleRender(q){if(renderTimer)clearTimeout(renderTimer);renderTimer=setTimeout(function(){requestPage(S.page,q);},180);}
function showPage(d){
  if(!d||d.doc!==docId||d.page!==S.page)return;
  S.reqSeq++;
  $('pagecount').textContent='/ '+S.pages;
  pimg.src=d.img;
  pimg.onload=function(){
    clearFail();
    var pw=d.w,ph=d.h;
    if(!S.baseW){S.baseW=pw;S.baseH=ph;}
    var want=curScaleMilli();
    if(S.pendingScale&&Math.abs(want-S.pendingScale)/S.pendingScale>0.05){
      S.pendingScale=0;
      requestPage(S.page,S.lastQ||null);
      return;
    }
    S.pendingScale=0;
    layoutOverlays(d);
    status('Page '+(S.page+1)+' of '+S.pages);
  };
  pimg.onerror=function(){fail('Page image failed to decode.');};
}
function layoutOverlays(d){
  var W=pimg.clientWidth,H=pimg.clientHeight;
  if(!W||!H)return;
  pageEl.style.width=W+'px';
  textL.textContent='';linkL.textContent='';matchL.textContent='';
  if(S.perms.copy&&d.lines&&d.lines.length){
    d.lines.forEach(function(L){
      var r=L.rect;if(!r)return;
      var s=el('span','ln',L.text);
      s.style.left=pct(r.x);s.style.top=pct(r.y);s.style.width=pct(r.w);s.style.height=pct(r.h);
      s.style.fontSize=Math.max(4,r.h*H)+'px';
      textL.appendChild(s);
    });
  }
  (d.links||[]).forEach(function(L){
    var r=L.rect;if(!r)return;
    var a=el('a');
    a.style.left=pct(r.x);a.style.top=pct(r.y);a.style.width=pct(r.w);a.style.height=pct(r.h);
    if(L.url){
      a.href='#';a.title=L.url+' (opens in a new tab)';
      (function(u){a.addEventListener('click',function(ev){ev.preventDefault();post({type:'pdf_link',doc:docId,url:u});});})(L.url);
    }
    else if(L.page!=null){
      a.href='#';
      a.title='Go to page '+(L.page+1);
      (function(p){a.addEventListener('click',function(ev){ev.preventDefault();requestPage(p);});})(L.page);
    }
    else{return;}
    linkL.appendChild(a);
  });
  var rects=(d.matches&&d.matches.length)?d.matches:(S.cachedRects||[]);
  S.cachedRects=[];
  rects.forEach(function(r){
    var m=el('div','m');
    m.style.left=pct(r.x);m.style.top=pct(r.y);m.style.width=pct(r.w);m.style.height=pct(r.h);
    matchL.appendChild(m);
  });
}
function markThumb(){
  var kids=$('pane-thumbs').children;
  for(var i=0;i<kids.length;i++)kids[i].classList.toggle('cur',i===S.page);
}
function buildThumbs(){
  var box=$('pane-thumbs');box.textContent='';
  for(var i=0;i<S.pages;i++){
    (function(p){
      var b=el('button','thumb');if(p===S.page)b.classList.add('cur');
      var im=el('img');im.alt='Page '+(p+1);im.dataset.page=p;
      var lab=el('span',null,String(p+1));
      b.appendChild(im);b.appendChild(lab);
      b.addEventListener('click',function(){requestPage(p);});
      box.appendChild(b);
    })(i);
  }
  if('IntersectionObserver' in window){
    var io=new IntersectionObserver(function(es){
      es.forEach(function(e){
        if(!e.isIntersecting)return;
        var im=e.target,p=+im.dataset.page;
        io.unobserve(im);
        if(im.src||S.thumbs[p])return;
        S.thumbs[p]=1;
        post({type:'pdf_page',doc:docId,page:p,scale:200,thumb:true});
      });
    },{root:$('sidebody')});
    var imgs=box.querySelectorAll('img');
    for(var k=0;k<imgs.length;k++)io.observe(imgs[k]);
  }
}
window.__ferritePdfThumb=function(d){
  if(!d||d.doc!==docId)return;
  var box=$('pane-thumbs').children[d.page];
  if(!box)return;
  var im=box.querySelector('img');
  if(im)im.src=d.img;
};
function buildOutline(list){
  var box=$('pane-outline');box.textContent='';
  if(!list||!list.length){box.appendChild(el('div','ro','No outline in this document.'));return;}
  list.forEach(function(e){
    var b=el('button','oline',e.title);
    b.style.paddingLeft=(8+((e.depth||0)*16))+'px';
    b.title='Page '+(((e.page|0))+1);
    (function(p){b.addEventListener('click',function(){requestPage(p);});})(e.page|0);
    box.appendChild(b);
  });
}
function buildFields(list){
  var box=$('pane-fields');box.textContent='';
  if(!list||!list.length){box.appendChild(el('div','ro','No form fields in this document.'));return;}
  list.forEach(function(f){
    var row=el('div','srow');
    row.appendChild(el('div','k',f.name||('Field '+(f.index+1))));
    row.appendChild(el('div','ro',f.kind+(f.editable?'':' - read-only')));
    if(f.value!=null&&f.kind!=='checkbox'&&f.kind!=='radio'){
      row.appendChild(el('div','ro',String(f.value)));
    }
    if(f.editable&&f.kind==='text'){
      var inp=el('input');inp.type='text';inp.value=f.value||'';inp.setAttribute('aria-label',f.name||'text field');
      var app=el('button',null,'Apply');
      app.addEventListener('click',function(){
        post({type:'pdf_fill',doc:docId,index:f.index,kind:'text',value:inp.value});
        status('Applying...');
      });
      row.appendChild(inp);row.appendChild(app);
    }else if(f.editable&&f.kind==='checkbox'){
      var lab=el('label');var cb=el('input');cb.type='checkbox';cb.checked=!!f.checked;
      cb.setAttribute('aria-label',f.name||'checkbox');
      cb.addEventListener('change',function(){
        post({type:'pdf_fill',doc:docId,index:f.index,kind:'check',checked:cb.checked});
      });
      lab.appendChild(cb);lab.appendChild(document.createTextNode('Checked'));
      row.appendChild(lab);
    }else if(f.editable&&f.kind==='radio'){
      var rb=el('button',null,f.checked?'Selected':'Select');
      rb.disabled=!!f.checked;
      rb.addEventListener('click',function(){
        post({type:'pdf_fill',doc:docId,index:f.index,kind:'radio'});
      });
      row.appendChild(rb);
    }else if((f.kind==='combo'||f.kind==='list')&&f.options&&f.options.length){
      var sel=el('select');sel.disabled=true;sel.setAttribute('aria-label',f.name||'choice');
      f.options.forEach(function(o){
        var op=el('option',null,o);
        if(o===f.value)op.selected=true;
        sel.appendChild(op);
      });
      row.appendChild(sel);
      row.appendChild(el('div','ro','Choice fields display the saved value.'));
    }
    box.appendChild(row);
  });
}
window.__ferritePdfDoc=function(d){
  if(!d||d.doc!==docId)return;
  S.reqSeq++;
  if(d.locked){
    S.locked=true;$('modal').classList.add('on');
    if(S.unlockTried){
      $('pwerr').textContent='Wrong password. Try again.';
      $('pwerr').classList.add('on');
    }
    setTimeout(function(){$('pwin').focus();},50);
    status('Locked.');
    return;
  }
  S.locked=false;$('modal').classList.remove('on');$('pwerr').classList.remove('on');
  var info=d.info||{};
  S.engine=info.engine!==false;
  S.pages=d.pages|0;
  var title=d.title||'PDF';
  document.title=title+' - Ferrite PDF';
  $('doctitle').textContent=title;
  $('pagecount').textContent='/ '+S.pages;
  var meta=info.meta||{};
  var bits=[];
  if(meta.author)bits.push('by '+meta.author);
  bits.push(S.pages+(S.pages===1?' page':' pages'));
  status(bits.join(' - '));
  S.perms=info.perms||{print:true,copy:true,fill:true};
  if(!S.engine){
    var b=$('banner');
    b.textContent='Rendering needs the pdfium-render engine (see docs/SETUP.md). Search, form values, and save still work.';
    b.classList.add('on');
  }
  if(!S.perms.print)$('bPrint').disabled=true;
  S.outline=d.outline||[];
  S.fields=d.fields||[];
  buildOutline(S.outline);
  buildFields(S.fields);
  buildThumbs();
  updateSave();
  requestPage(0);
};
window.__ferritePdfPage=function(d){if(d&&!d.thumb)showPage(d);};
window.__ferritePdfSearch=function(d){
  if(!d||d.doc!==docId)return;
  var box=$('sres');box.textContent='';
  var total=d.total|0;
  var head=el('div','ro',(total===0?'No matches.':total+' match'+(total===1?'':'es')+(d.truncated?' (first pages)':'')+' - click to jump.'));
  head.style.marginBottom='8px';box.appendChild(head);
  (d.hits||[]).forEach(function(h){
    var n=(h.rects||[]).length;
    var b=el('button','sres','Page '+(h.page+1)+' - '+n+' match'+(n===1?'':'es'));
    (function(pg,rc){b.addEventListener('click',function(){
      S.cachedRects=rc||[];
      requestPage(pg);
    });})(h.page,h.rects);
    box.appendChild(b);
  });
  status(total+' match'+(total===1?'':'es')+'.');
};
window.__ferritePdfForms=function(d){
  if(!d||d.doc!==docId)return;
  S.fields=d.fields||[];
  buildFields(S.fields);
  S.dirty=true;updateSave();
  status('Field applied.');
};
window.__ferritePdfSaved=function(d){
  if(!d||d.doc!==docId)return;
  S.dirty=false;updateSave();
  status('Saved '+(d.file||'copy')+'.');
};
window.__ferritePdfError=function(d){
  if(!d||d.doc!==docId)return;
  fail(d.error||'Page failed to render.');
};
function updateSave(){$('bSave').disabled=!S.dirty;}
function gotoDelta(n){requestPage(S.page+n);}
function gotoInput(){
  var n=parseInt($('pagein').value,10);
  if(!isNaN(n))requestPage(n-1);
  else $('pagein').value=String(S.page+1);
}
function setZoom(v){
  if(v==='fit-w'||v==='fit-h'){S.mode=v;}
  else{S.mode='custom';S.custom=Math.max(25,Math.min(400,parseInt(v,10)||100));}
  scheduleRender();
}
function bindTabs(){
  var tabs=$('sidetabs').querySelectorAll('button');
  for(var i=0;i<tabs.length;i++)(function(t){
    t.addEventListener('click',function(){
      var all=$('sidetabs').querySelectorAll('button');
      for(var a=0;a<all.length;a++)all[a].classList.remove('on');
      t.classList.add('on');
      var panes=$('sidebody').querySelectorAll('.pane');
      for(var q=0;q<panes.length;q++)panes[q].classList.remove('on');
      $('pane-'+t.getAttribute('data-pane')).classList.add('on');
    });
  })(tabs[i]);
}
function boot(){
  try{
    docId=new URLSearchParams(location.search).get('doc')||'';
  }catch(e){docId='';}
  if(!docId){fail('No document. Open a PDF to view it here.');return;}
  if(window.innerWidth<900)$('side').classList.add('hidden');
  $('bSide').addEventListener('click',function(){$('side').classList.toggle('hidden');});
  $('bPrev').addEventListener('click',function(){gotoDelta(-1);});
  $('bNext').addEventListener('click',function(){gotoDelta(1);});
  $('pagein').addEventListener('change',gotoInput);
  $('bZout').addEventListener('click',function(){
    if(S.mode==='custom')setZoom(String(S.custom-25));
    else{$('zoom').value='100';setZoom('100');}
  });
  $('bZin').addEventListener('click',function(){
    if(S.mode==='custom')setZoom(String(S.custom+25));
    else{$('zoom').value='100';setZoom('100');}
  });
  $('zoom').addEventListener('change',function(e){setZoom(e.target.value);});
  $('bSearch').addEventListener('click',function(){
    $('side').classList.remove('hidden');
    var tabs=$('sidetabs').querySelectorAll('button');
    for(var a=0;a<tabs.length;a++)tabs[a].classList.remove('on');
    tabs[2].classList.add('on');
    var panes=$('sidebody').querySelectorAll('.pane');
    for(var q=0;q<panes.length;q++)panes[q].classList.remove('on');
    $('pane-search').classList.add('on');
    $('searchq').focus();
  });
  $('bPrint').addEventListener('click',function(){post({type:'print'});});
  $('bSave').addEventListener('click',function(){post({type:'pdf_save',doc:docId});status('Saving...');});
  $('bGo').addEventListener('click',doSearch);
  $('searchq').addEventListener('keydown',function(e){if(e.key==='Enter')doSearch();});
  $('bRetry').addEventListener('click',function(){clearFail();requestPage(S.page);});
  $('bUnlock').addEventListener('click',doUnlock);
  $('pwin').addEventListener('keydown',function(e){if(e.key==='Enter')doUnlock();});
  $('bCancel').addEventListener('click',function(){
    $('modal').classList.remove('on');$('pwin').value='';
    fail('This document is encrypted. Unlock it to read.');
  });
  document.addEventListener('keydown',function(e){
    var tag=(e.target&&e.target.tagName)||'';
    var typing=tag==='INPUT'||tag==='SELECT'||tag==='TEXTAREA';
    if((e.ctrlKey||e.metaKey)&&e.key.toLowerCase()==='f'){e.preventDefault();$('bSearch').click();return;}
    if((e.ctrlKey||e.metaKey)&&e.key.toLowerCase()==='p'){e.preventDefault();if(!$('bPrint').disabled)post({type:'print'});return;}
    if(typing)return;
    if(e.key==='ArrowRight'||e.key==='PageDown')gotoDelta(1);
    else if(e.key==='ArrowLeft'||e.key==='PageUp')gotoDelta(-1);
    else if(e.key==='Home')requestPage(0);
    else if(e.key==='End')requestPage(S.pages-1);
    else if(e.key==='+'||e.key==='=')$('bZin').click();
    else if(e.key==='-')$('bZout').click();
    else if(e.key==='Escape'){$('modal').classList.remove('on');}
  });
  window.addEventListener('resize',function(){scheduleRender();});
  bindTabs();
  status('Opening...');
  post({type:'pdf_doc',doc:docId});
}
function doSearch(){
  var q=$('searchq').value||'';
  if(!q){status('Type something to find.');return;}
  if(q.length>256){status('Query too long.');return;}
  post({type:'pdf_search',doc:docId,q:q,case:$('casebox').checked});
  status('Searching...');
}
function doUnlock(){
  var pw=$('pwin').value||'';
  if(!pw){$('pwerr').textContent='Enter the password.';$('pwerr').classList.add('on');return;}
  S.unlockTried=true;
  post({type:'pdf_unlock',doc:docId,password:pw});
  $('pwin').value='';
  status('Unlocking...');
}
boot();
})();
</script></body></html>
"##;
/// Downloads library: durable vault rows (grouped by day) with per-row
/// actions. All row text via `textContent`; destructive choice is explicit
/// (remove-from-list vs file delete, which has no button here by design,
/// deleting bytes stays a deliberate OS-level act for now).
pub const DOWNLOADS_HTML: &str = concat!(
    "<!DOCTYPE html><!-- ferrite://downloads -->",
    "<html><head><meta charset=\"utf-8\"><title>Downloads - Ferrite</title>",
    "<style>/*__FERRITE_THEME__*/",
    "body{background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui);margin:0;padding:32px;}",
    "h1{font-size:20px;margin:0 0 16px;}",
    "#q{background:var(--f-surface-1);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-card-radius,8px);padding:8px 12px;width:min(480px,90%);font-size:14px;margin-bottom:16px;}",
    ".day{color:var(--f-muted);font-size:12px;letter-spacing:.08em;text-transform:uppercase;margin:20px 0 4px;}",
    ".row{padding:10px 4px;border-bottom:1px solid var(--f-border);}",
    ".row .top{display:flex;gap:12px;align-items:baseline;}",
    ".row .fn{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-weight:600;}",
    ".row .meta{color:var(--f-muted);font-size:12px;margin-top:2px;}",
    ".row .url{color:var(--f-muted);font-size:12px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}",
    ".row .hash{color:var(--f-faint);font-size:11px;font-family:var(--f-font-mono);cursor:pointer;}",
    ".warn{color:var(--f-danger);font-size:11px;font-weight:700;margin-left:8px;}",
    ".acts{display:flex;gap:8px;margin-top:6px;flex-wrap:wrap;}",
    ".acts button{background:none;border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-ctl-radius,6px);padding:3px 10px;cursor:pointer;font-size:12px;}",
    ".acts button:hover{background:var(--f-surface-1);}",
    ".row.flash{outline:2px solid var(--f-accent);outline-offset:-2px;}",
    "@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important;}}</style></head>",
    "<body><h1>Downloads</h1>",
    "<input id=\"q\" placeholder=\"Filter downloads\" autocomplete=\"off\" spellcheck=\"false\"><div id=\"list\"></div><div id=\"sr-live\" aria-live=\"polite\" style=\"position:absolute;left:-9999px;\"></div>",
    "<script>function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}",
    "function fmtSize(n){if(n==null)return'';if(n<1024)return n+' B';if(n<1048576)return (n/1024).toFixed(1)+' KB';if(n<1073741824)return (n/1048576).toFixed(1)+' MB';return (n/1073741824).toFixed(2)+' GB';}",
    "function dayOf(ts){var d=new Date(ts),t=new Date();t.setHours(0,0,0,0);var dd=new Date(d);dd.setHours(0,0,0,0);var diff=Math.round((t-dd)/86400000);if(diff<=0)return'Today';if(diff===1)return'Yesterday';return d.toLocaleDateString();}",
    "window['__ferriteDownloads']=function(items){",
    "var l=document.getElementById('list'),live=document.getElementById('sr-live');",
    // Keyed rows: nodes persist by download id across 500ms pushes (no
    // remount, no focus loss); only changed signatures rebuild children.
    // The map lives on the list element (per-document, two open downloads
    // tabs must never share nodes).
    "var rows=l.__dlRows||(l.__dlRows={});",
    "var seen={},announce=[],flashEl=null;",
    "var wantFlash=window.__dlFlash||'';",
    "(items||[]).forEach(function(it){",
    "var key='r:'+(it.id||it.url);seen[key]=1;",
    "var sig=(it.state||'')+'|'+(it.size||'')+'|'+(it.hash||'');",
    "var r=rows[key],isNew=!r;",
    "if(r&&r._sig===sig&&r._danger===!!it.danger){}",
    "else{r=r||document.createElement('div');r.className='row';r.setAttribute('data-did',it.id||'');fillRow(r,it);r._sig=sig;r._danger=!!it.danger;rows[key]=r;",
    // Completion transitions announce once (polite); ticking progress
    // never touches the live region (continuous spam). First sight of an
    // already-terminal row stays silent. Only transitions announce.
    "if(!isNew&&r._announced!==it.state&&(it.state==='Completed'||it.state==='Failed')){announce.push((it.state==='Completed'?'Download complete: ':'Download failed: ')+(it.filename||it.url||''));}",
    "r._announced=it.state;}",
    "if(wantFlash&&it.id===wantFlash)flashEl=r;",
    "});",
    "for(var k in rows){if(!seen[k]){rows[k].remove();delete rows[k];}}",
    // Re-append only when the order actually changed: same-order pushes
    // leave focused buttons alone (500ms steady state costs nothing).
    "var order=(items||[]).map(function(it){return it.id||it.url;}).join('\\u0001');",
    "if(order!==l.__dlOrder){l.__dlOrder=order;",
    "l.textContent='';",
    "var lastDay='';(items||[]).forEach(function(it){",
    "var r=rows['r:'+(it.id||it.url)];if(!r)return;",
    "var day=it.ts?dayOf(it.ts):'Older';if(day!==lastDay){lastDay=day;var dh=document.createElement('div');dh.className='day';dh.textContent=day;l.appendChild(dh);}",
    "l.appendChild(r);});",
    "if(!items||!items.length){var e=document.createElement('div');e.style.cssText='color:var(--f-muted);padding:16px 4px;';e.textContent='No downloads yet';l.appendChild(e);}",
    "}",
    "if(announce.length)live.textContent=announce.join('. ');",
    "if(flashEl){window.__dlFlash='';flashEl.classList.add('flash');try{flashEl.scrollIntoView({block:'center'});}catch(_){}setTimeout(function(){flashEl.classList.remove('flash');},2500);}",
    "};",
    "function fillRow(r,it){r.textContent='';",
    "var top=document.createElement('div');top.className='top';",
    "var fn=document.createElement('span');fn.className='fn';fn.textContent=it.filename||'';top.appendChild(fn);",
    "if(it.danger){var w=document.createElement('span');w.className='warn';w.textContent='EXECUTABLE \\u2014 confirm before opening';top.appendChild(w);}",
    "var st=document.createElement('span');st.className='meta';st.textContent=it.state||'';top.appendChild(st);",
    "r.appendChild(top);",
    "var meta=document.createElement('div');meta.className='meta';",
    "meta.textContent=[fmtSize(it.size),it.ts?new Date(it.ts).toLocaleString():null].filter(Boolean).join(' \\u00b7 ');r.appendChild(meta);",
    "var u=document.createElement('div');u.className='url';u.textContent=it.url||'';u.title=it.url||'';r.appendChild(u);",
    "if(it.hash){var hh=document.createElement('div');hh.className='hash';hh.textContent='sha256:'+it.hash;hh.title='Click to copy';hh.onclick=function(){post({type:'dl_copy',text:it.hash});};r.appendChild(hh);}",
    "var a=document.createElement('div');a.className='acts';",
    "function btn(label,msg){var b=document.createElement('button');b.textContent=label;b.onclick=function(ev){ev.stopPropagation();post(msg);};a.appendChild(b);}",
    "if(it.filename){btn('Open',{type:'dl_open',path:it.filename});btn('Show in folder',{type:'dl_folder',path:it.filename});}",
    "if(it.url)btn('Copy link', {type:'dl_copy',text:it.url});",
    "if(it.url&&(it.state==='Failed'||it.state==='Cancelled'))btn('Retry',{type:'dl_retry',url:it.url});",
    "if(it.id)btn('Remove from list',{type:'dl_remove',id:it.id});",
    "r.appendChild(a);}",
    "var q=document.getElementById('q');var dlStatus='';",
    // Query-param contract (?q=, ?status=, ?id=): deep links preset and
    // highlight; typing only replaces state, Enter commits a real nav.
    "try{var sp=new URLSearchParams(location.search);var pq=sp.get('q')||'';var ps=sp.get('status')||'';window.__dlFlash=sp.get('id')||'';if(pq)q.value=pq;if(ps)dlStatus=ps;}catch(_){}",
    "function curQuery(){return {type:'downloads_query',query:q.value,status:dlStatus};}",
    "q.addEventListener('input',function(){post(curQuery());",
    "try{history.replaceState(null,'',q.value?('?q='+encodeURIComponent(q.value)):location.pathname);}catch(_){}",
    "});",
    "q.addEventListener('keydown',function(e){if(e.key==='Escape')post({type:'dismiss'});",
    "if(e.key==='Enter')post({type:'downloads_commit',query:q.value,status:dlStatus});});",
    "post(curQuery());</script></body></html>"
);
/// Bookmarks manager: two-pane tree + table over the same vault the bar
/// and panel read (live fan-out keeps all three in sync, no surface owns
/// a copy). Query contract: `?folder=` selects, `?q=` searches,
/// `?highlight=` flashes one row. Typing only replaces state; Enter
/// commits a real navigation.
pub const BOOKMARKS_HTML: &str = r#"<!DOCTYPE html><!-- ferrite://bookmarks -->
<html><head><meta charset="utf-8"><title>Bookmarks - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;padding:0;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
#wrap{padding:24px 32px;max-width:1100px}
h1{font-size:20px;margin:0 0 12px}h2{font-size:13px;letter-spacing:.06em;text-transform:uppercase;color:var(--f-muted);font-weight:400}
#q{background:var(--f-surface-1);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-card-radius,8px);padding:8px 12px;width:min(480px,90%);font-size:14px;margin-bottom:16px}
#mgr{display:flex;gap:16px;align-items:flex-start}
#tree{width:260px;flex:none;background:var(--f-surface-1);border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);padding:6px;max-height:70vh;overflow-y:auto}
#tree .tn{display:flex;align-items:center;gap:6px;padding:6px 8px;border-radius:var(--pk-card-radius,8px);cursor:pointer;font-size:13px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
#tree .tn:hover{background:var(--f-surface-2)}
#tree .tn.sel{background:var(--f-surface-2);box-shadow:inset 2px 0 0 var(--f-accent)}
#tree .tn .tw{flex:none;width:14px;color:var(--f-faint);font-size:10px}
#tree .tn svg.ic{width:14px;height:14px;flex:none;color:var(--f-muted)}
#main{flex:1;min-width:0}
#toolbar{display:flex;gap:8px;align-items:center;margin-bottom:8px;flex-wrap:wrap}
#toolbar select,#toolbar input{background:var(--f-surface-1);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-card-radius,8px);padding:6px 8px;font-size:12px;font-family:inherit}
#toolbar button{background:none;border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-ctl-radius,6px);padding:5px 10px;cursor:pointer;font-size:12px;font-family:inherit}
#toolbar button:hover{background:var(--f-surface-1)}
#toolbar button:disabled{opacity:.4;cursor:default}
#rows{border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);overflow:hidden}
#rows .r{display:flex;gap:10px;align-items:center;padding:8px 10px;border-bottom:1px solid var(--f-border);font-size:13px}
#rows .r:last-child{border-bottom:none}
#rows .r:hover{background:var(--f-surface-1)}
#rows .r .av{width:20px;height:20px;border-radius:50%;background:var(--f-surface-2);color:var(--f-muted);font-size:10px;font-weight:700;display:flex;align-items:center;justify-content:center;flex:none}
#rows .r .t{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;cursor:pointer}
#rows .r .u{color:var(--f-faint);font-size:11px;max-width:32%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;flex:none}
#rows .r .d{color:var(--f-faint);font-size:11px;flex:none;font-variant-numeric:tabular-nums}
#rows .r button{background:none;border:1px solid var(--f-border);color:var(--f-muted);border-radius:var(--pk-ctl-radius,6px);padding:2px 8px;cursor:pointer;font-size:11px;flex:none;font-family:inherit}
#rows .r button:hover{color:var(--f-fg)}
#rows .r.flash{outline:2px solid var(--f-accent);outline-offset:-2px}
#rows input.rename{background:var(--f-surface-2);border:1px solid var(--f-accent);color:var(--f-fg);border-radius:var(--pk-ctl-radius,6px);padding:2px 6px;font-size:13px;font-family:inherit;flex:1;min-width:0}
.dup{border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);padding:10px 12px;margin-bottom:8px;font-size:13px}
.dup .du{color:var(--f-muted);font-size:12px;margin-bottom:6px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.dup .di{display:flex;gap:8px;align-items:center;padding:3px 0}
.dup .di span{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.dup .di button{background:none;border:1px solid var(--f-border);color:var(--f-muted);border-radius:var(--pk-ctl-radius,6px);padding:2px 8px;cursor:pointer;font-size:11px;flex:none;font-family:inherit}
.empty{color:var(--f-faint);font-size:13px;padding:16px 4px}
#skip{position:absolute;left:-9999px;top:8px;background:var(--f-surface-1);border:1px solid var(--f-accent);color:var(--f-fg);padding:8px 12px;border-radius:var(--pk-ctl-radius,6px);font-size:13px;font-family:inherit;cursor:pointer}
#skip:focus{left:8px}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}</style></head>
<body><div id="wrap">
<button id="skip">Skip to search</button>
<h1>Bookmarks</h1>
<input id="q" placeholder="Search bookmarks" autocomplete="off" spellcheck="false">
<div id="mgr">
<div id="tree" role="tree" aria-label="Bookmark folders"></div>
<div id="main">
<div id="toolbar">
<select id="sort" aria-label="Sort order"><option value="manual">Manual order</option><option value="name">Name</option><option value="date">Date added</option></select>
<select id="moveto" aria-label="Move destination"></select>
<button id="mvbtn" disabled>Move selected</button>
<button id="delbtn" disabled>Delete selected</button>
<input id="fname" type="text" placeholder="New folder" size="14" aria-label="New folder name">
<button id="newfolder">New folder</button>
</div>
<div id="rows" role="grid" aria-label="Bookmarks"></div>
</div>
</div>
<h2>Duplicates</h2>
<div id="dups"></div>
</div>
<script>
(function(){
  var q=document.getElementById('q'),tree=document.getElementById('tree'),
      rowsEl=document.getElementById('rows'),dupsEl=document.getElementById('dups'),
      sortEl=document.getElementById('sort'),moveEl=document.getElementById('moveto');
  var folders=[],children=[],selFolder=null,selRows={},sortMode='manual',lastIdx=-1;
  function post(o){try{window.ipc.postMessage(JSON.stringify(o));}catch(e){}}
  function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');}
  function avatarOf(t){var m=String(t||'').match(/[A-Za-z0-9]/);return m?m[0].toUpperCase():'•';}
  function folderName(id){if(!id)return'Bookmarks bar';for(var i=0;i<folders.length;i++)if(folders[i].id===id)return folders[i].title;return'(deleted folder)';}
  function nest(){
    // parent id -> child folders, in pushed order.
    var byParent={},i,f;
    for(i=0;i<folders.length;i++){f=folders[i];var p=f.parent||'';(byParent[p]=byParent[p]||[]).push(f);}
    return byParent;
  }
  function renderTree(){
    // Deleted-selection fallback: a removed folder collapses to the root
    // instead of displaying a node that no longer exists.
    if(selFolder&&!folders.some(function(f){return f.id===selFolder;})){
      selFolder=null;
      try{history.replaceState(null,'',location.pathname);}catch(_){}
      post({type:'bookmarks_children',folder:null});
    }
    var byParent=nest(),h='';
    function walk(pid,depth){
      (byParent[pid||'']||[]).forEach(function(f){
        var kids=byParent[f.id]||[];
        var open=treeOpen[f.id]!==false;
        h+='<div class="tn'+(selFolder===f.id?' sel':'')+'" role="treeitem" aria-expanded="'+(kids.length?(open?'true':'false'):'false')+'" aria-selected="'+(selFolder===f.id)+'" tabindex="-1" data-fid="'+f.id+'" style="padding-left:'+(8+depth*16)+'px">'
          +'<span class="tw">'+(kids.length?(open?'-':'+'):'')+'</span><span>'+esc(f.title)+'</span></div>';
        if(open)walk(f.id,depth+1);
      });
    }
    h='<div class="tn'+(selFolder===null?' sel':'')+'" role="treeitem" aria-selected="'+(selFolder===null)+'" tabindex="-1" data-fid="">'
      +'<span class="tw"></span><span>Bookmarks bar</span></div>';
    walk(null,0);
    tree.innerHTML=h;
    // Move destinations follow the same tree.
    var mh='<option value="">Bookmarks bar</option>';
    folders.forEach(function(f){mh+='<option value="'+f.id+'">'+esc(f.title)+'</option>';});
    moveEl.innerHTML=mh;
  }
  var treeOpen={};
  function visibleNodes(){return Array.prototype.slice.call(tree.querySelectorAll('[data-fid]'));}
  function selectFolder(id){
    selFolder=id||null;selRows={};lastIdx=-1;refreshBulk();
    try{history.replaceState(null,'',selFolder?('?folder='+encodeURIComponent(selFolder)):location.pathname);}catch(_){}
    post({type:'bookmarks_children',folder:selFolder});
    renderTree();
  }
  tree.addEventListener('click',function(e){
    var n=e.target.closest('[data-fid]');if(!n)return;
    var id=n.getAttribute('data-fid')||null;
    // Chevron toggles expansion without selecting.
    if(e.target.classList&&e.target.classList.contains('tw')&&id){treeOpen[id]=treeOpen[id]===false?true:false;renderTree();return;}
    selectFolder(id);
  });
  // File-manager arrows: Up/Down move, Right expands, Left collapses.
  tree.addEventListener('keydown',function(e){
    var nodes=visibleNodes(),idx=nodes.indexOf(document.activeElement);
    if(e.key==='ArrowDown'||e.key==='ArrowUp'){
      e.preventDefault();
      nodes[e.key==='ArrowDown'?(idx+1)%nodes.length:(idx-1+nodes.length)%nodes.length].focus();
    } else if(e.key==='Enter'&&idx>=0){e.preventDefault();nodes[idx].click();}
    else if(e.key==='ArrowRight'&&idx>=0){
      var id=nodes[idx].getAttribute('data-fid');
      if(id&&treeOpen[id]===false){e.preventDefault();treeOpen[id]=true;renderTree();}
    } else if(e.key==='ArrowLeft'&&idx>=0){
      var id2=nodes[idx].getAttribute('data-fid');
      if(id2&&treeOpen[id2]!==false){e.preventDefault();treeOpen[id2]=false;renderTree();}
    }
  });
  function sortedRows(){
    var list=children.slice();
    if(sortMode==='name')list.sort(function(a,b){return (a.title||a.url).toLowerCase()<(b.title||b.url).toLowerCase()?-1:1;});
    else if(sortMode==='date')list.sort(function(a,b){return (b.added||0)-(a.added||0);});
    return list;
  }
  function refreshBulk(){
    var n=Object.keys(selRows).length;
    document.getElementById('mvbtn').disabled=!n;
    document.getElementById('delbtn').disabled=!n;
  }
  function renderRows(){
    var h='',list=sortedRows();
    list.forEach(function(it,i){
      var label=it.title||it.url||'';
      h+='<div class="r'+(flashId&&it.id===flashId?' flash':'')+'" data-rid="'+it.id+'" data-idx="'+i+'">'
        +'<input type="checkbox" data-check="'+it.id+'"'+(selRows[it.id]?' checked':'')+' aria-label="Select '+esc(label)+'">'
        +'<span class="av">'+esc(avatarOf(label))+'</span>'
        +'<span class="t" data-open="'+esc(it.url||'')+'" title="'+esc(label)+'">'+esc(label)+'</span>'
        +(it.url?'<span class="u">'+esc(it.url)+'</span>':'<span class="u">folder</span>')
        +'<span class="d">'+(it.added?new Date(it.added).toLocaleDateString():'')+'</span>'
        +'<button data-ren="'+it.id+'" title="Rename">Rename</button>'
        +(it.url?'<button data-delone="'+it.id+'" title="Delete">Delete</button>':'')+'</div>';
    });
    rowsEl.innerHTML=h||'<div class="empty">Empty, save pages with Ctrl+D.</div>';
    if(flashId){
      var f=rowsEl.querySelector('[data-rid="'+flashId+'"]');
      flashId=null;
      if(f){try{f.scrollIntoView({block:'center'});}catch(_){}setTimeout(function(){f.classList.remove('flash');},2500);}
    }
  }
  var flashId='';
  rowsEl.addEventListener('click',function(e){
    var c=e.target.closest?e.target.closest('[data-check]'):null;
    if(c){var id=c.getAttribute('data-check');if(c.checked)selRows[id]=1;else delete selRows[id];refreshBulk();return;}
    var o=e.target.closest?e.target.closest('[data-open]'):null;
    if(o&&o.getAttribute('data-open')){post({type:'navigate',url:o.getAttribute('data-open')});return;}
    var rn=e.target.closest?e.target.closest('[data-ren]'):null;
    if(rn){startRename(rn.getAttribute('data-ren'));return;}
    var d=e.target.closest?e.target.closest('[data-delone]'):null;
    if(d){post({type:'bookmarks_delete_many',ids:[d.getAttribute('data-delone')]});return;}
    var r=e.target.closest?e.target.closest('[data-rid]'):null;
    if(r&&e.shiftKey){
      // Shift-click range from the last checked row.
      var list=sortedRows(),ids=list.map(function(it){return it.id;});
      var cur=ids.indexOf(r.getAttribute('data-rid'));
      if(lastIdx>=0&&cur>=0){
        var a=Math.min(lastIdx,cur),b=Math.max(lastIdx,cur),i;
        for(i=a;i<=b;i++)selRows[ids[i]]=1;
        renderRows();refreshBulk();
      }
    }
  });
  rowsEl.addEventListener('change',function(e){
    var c=e.target.closest?e.target.closest('[data-check]'):null;
    if(!c)return;
    var list=sortedRows(),ids=list.map(function(it){return it.id;});
    lastIdx=ids.indexOf(c.getAttribute('data-check'));
  });
  function startRename(id){
    var row=rowsEl.querySelector('[data-rid="'+id+'"]');
    var t=row?row.querySelector('.t'):null;
    if(!t)return;
    var cur=t.textContent,input=document.createElement('input');
    input.className='rename';input.value=cur;input.setAttribute('aria-label','New name');
    t.replaceWith(input);input.focus();input.select();
    function commit(save){
      var v=input.value.trim();
      if(save&&v&&v!==cur)post({type:'bookmarks_rename',id:id,title:v});
      else renderRows();
    }
    input.addEventListener('keydown',function(e){
      if(e.key==='Enter')commit(true);
      else if(e.key==='Escape')commit(false);
      e.stopPropagation();
    });
    input.addEventListener('blur',function(){commit(true);});
  }
  document.getElementById('mvbtn').addEventListener('click',function(){
    var ids=Object.keys(selRows);
    if(!ids.length)return;
    var dest=moveEl.value||null;
    ids.forEach(function(id){post({type:'bookmarks_move',id:id,parent:dest});});
    selRows={};refreshBulk();
  });
  document.getElementById('delbtn').addEventListener('click',function(){
    var ids=Object.keys(selRows);
    if(!ids.length)return;
    post({type:'bookmarks_delete_many',ids:ids});
    selRows={};refreshBulk();
  });
  document.getElementById('newfolder').addEventListener('click',function(){
    var nm=document.getElementById('fname').value.trim();
    if(!nm)return;
    post({type:'bookmarks_add_folder',name:nm,parent:selFolder});
    document.getElementById('fname').value='';
  });
  document.getElementById('dups').addEventListener('click',function(e){
    var d=e.target.closest?e.target.closest('[data-dupdel]'):null;
    if(d)post({type:'bookmarks_delete_many',ids:[d.getAttribute('data-dupdel')]});
  });
  sortEl.addEventListener('change',function(){sortMode=sortEl.value;renderRows();});
  window.__ferriteBmTree=function(f){
    folders=f||[];
    renderTree();
    var mh='<option value="">Bookmarks bar</option>';
    folders.forEach(function(fo){mh+='<option value="'+fo.id+'">'+esc(fo.title)+'</option>';});
    moveEl.innerHTML=mh;
  };
  window.__ferriteBmChildren=function(list){
    children=list||[];
    renderRows();
  };
  window.__ferriteBmDups=function(groups){
    groups=groups||[];
    var h='';
    groups.forEach(function(g){
      h+='<div class="dup"><div class="du">'+esc(g.url)+'</div>';
      g.items.forEach(function(it){
        h+='<div class="di"><span>'+esc(it.title||it.url)+', '+esc(folderName(it.parent))+'</span><button data-dupdel="'+it.id+'">Delete</button></div>';
      });
      h+='</div>';
    });
    dupsEl.innerHTML=h||'<div class="empty">No duplicates found.</div>';
  };
  document.getElementById('skip').addEventListener('click',function(){q.focus();q.select();});
  q.addEventListener('input',function(){
    post({type:'bookmarks_search',query:q.value});
    try{history.replaceState(null,'',q.value?('?q='+encodeURIComponent(q.value)):location.pathname);}catch(_){}
  });
  q.addEventListener('keydown',function(e){
    if(e.key==='Escape')post({type:'dismiss'});
    else if(e.key==='Enter')post({type:'bookmarks_commit',query:q.value});
  });
  // Query contract (?folder=, ?q=, ?highlight=): deep links preset state.
  try{
    var sp=new URLSearchParams(location.search);
    var fq=sp.get('q')||'';
    var ff=sp.get('folder');
    flashId=sp.get('highlight')||'';
    if(fq)q.value=fq;
    if(ff)selFolder=ff;
  }catch(_){}
  post({type:'bookmarks_tree'});
  if(q.value)post({type:'bookmarks_search',query:q.value});
  else post({type:'bookmarks_children',folder:selFolder});
  post({type:'bookmarks_dups'});
})();
</script></body></html>
"#;

/// Setting catalogue: the single registry the settings nav, the
/// client-side search index, and the shell value-push all read from.
/// Every entry names a control that exists in [`SETTINGS_HTML`],
/// enforced by test, so search can never silently miss a setting.
/// `label` is the display title (same text as the page heading) for
/// consumers that list settings outside the page, like the palette.
pub struct SettingDef {
    pub id: &'static str,
    pub category: &'static str,
    pub keywords: &'static str,
    pub label: &'static str,
}

pub const SETTINGS_CATALOGUE: &[SettingDef] = &[
    SettingDef {
        id: "privacy-adblock",
        category: "privacy",
        keywords: "adblock ads block trackers easylist filter",
        label: "Adblock (EasyList)",
    },
    SettingDef {
        id: "privacy-gpc",
        category: "privacy",
        keywords: "gpc opt out sale share privacy signal",
        label: "Send Sec-GPC: 1",
    },
    SettingDef {
        id: "privacy-realtime",
        category: "privacy",
        keywords: "real-time realtime safe browsing hash lookup threat malware phishing",
        label: "Real-time hash-prefix lookup",
    },
    SettingDef {
        id: "privacy-doh",
        category: "privacy",
        keywords: "doh dns encrypted hostname endpoint query private resolver",
        label: "Encrypted DNS (DoH)",
    },
    SettingDef {
        id: "privacy-https",
        category: "privacy",
        keywords: "https upgrade tls ssl http encrypted",
        label: "HTTPS upgrade",
    },
    SettingDef {
        id: "privacy-passwords",
        category: "privacy",
        keywords: "password manager credentials totp login",
        label: "Password manager",
    },
    SettingDef {
        id: "privacy-clear",
        category: "privacy",
        keywords: "clear browsing data history downloads delete erase range hour day week",
        label: "Clear browsing data",
    },
    SettingDef {
        id: "privacy-retention",
        category: "privacy",
        keywords: "history retention keep delete old visits automatically expire",
        label: "History retention",
    },
    SettingDef {
        id: "privacy-exclusions",
        category: "privacy",
        keywords: "excluded exclusions never save history per-site origin site exceptions",
        label: "Excluded sites",
    },
    SettingDef {
        id: "privacy-perms",
        category: "privacy",
        keywords: "permissions camera microphone location notifications clipboard allowed always revoke grants site permissions",
        label: "Site permissions",
    },
    SettingDef {
        id: "privacy-rotation",
        category: "privacy",
        keywords: "rotate rotation encryption key rekey re-encrypt master",
        label: "Encryption key",
    },
    SettingDef {
        id: "privacy-recovery",
        category: "privacy",
        keywords: "recovery phrase mnemonic backup restore key lost keyring",
        label: "Recovery phrase",
    },
    SettingDef {
        id: "search-engine",
        category: "search",
        keywords: "search engine default duckduckgo google bing brave",
        label: "Default search engine",
    },
    SettingDef {
        id: "search-custom",
        category: "search",
        keywords: "custom search engine add template query url",
        label: "Custom search engines",
    },
    SettingDef {
        id: "search-suggestions",
        category: "search",
        keywords: "suggestions autocomplete omnibox keystroke remote duckduckgo",
        label: "Autocomplete suggestions",
    },
    SettingDef {
        id: "search-home",
        category: "search",
        keywords: "home homepage start page button destination",
        label: "Home button destination",
    },
    SettingDef {
        id: "appearance-pack",
        category: "appearance",
        keywords: "theme appearance dark light pack volcanic tenebra",
        label: "Theme",
    },
    SettingDef {
        id: "appearance-tabs",
        category: "appearance",
        keywords: "tabs vertical horizontal strip rail sidebar layout",
        label: "Vertical tabs",
    },
    SettingDef {
        id: "language-spellcheck",
        category: "language",
        keywords: "spellcheck spelling suggestions hunspell dictionary dictionaries red squiggle text fields misspelled",
        label: "Spelling suggestions",
    },
    SettingDef {
        id: "language-langs",
        category: "language",
        keywords: "spellcheck languages list codes tags dictionary active consult",
        label: "Spellcheck languages",
    },
    SettingDef {
        id: "language-translate",
        category: "language",
        keywords: "translate translation on-device local language page selection translator",
        label: "On-device translation",
    },
    SettingDef {
        id: "language-translate-target",
        category: "language",
        keywords: "translate target language destination default translator",
        label: "Translation target language",
    },
    SettingDef {
        id: "downloads-page",
        category: "downloads",
        keywords: "downloads folder save files library",
        label: "Download library",
    },
    SettingDef {
        id: "startup-mode",
        category: "startup",
        keywords: "startup start launch open restore session new tab pages on startup",
        label: "On startup",
    },
    SettingDef {
        id: "startup-urls",
        category: "startup",
        keywords: "startup pages urls list open specific sites launch",
        label: "Startup pages",
    },
    SettingDef {
        id: "startup-picker",
        category: "startup",
        keywords: "startup profile picker ask who browsing choose launch",
        label: "Profile picker",
    },
    SettingDef {
        id: "performance-sleep",
        category: "performance",
        keywords: "sleep sleeping tabs inactive unload discard memory timeout performance",
        label: "Sleep inactive tabs",
    },
    SettingDef {
        id: "performance-exceptions",
        category: "performance",
        keywords: "sleep exceptions never sleep keep awake sites whitelist memory",
        label: "Never sleep these sites",
    },
    SettingDef {
        id: "autofill-address",
        category: "autofill",
        keywords: "autofill address form fill street postal",
        label: "Address autofill",
    },
    SettingDef {
        id: "autofill-payment",
        category: "autofill",
        keywords: "autofill payment card credit fill checkout",
        label: "Payment autofill",
    },
    SettingDef {
        id: "autofill-form",
        category: "autofill",
        keywords: "autofill form history remember typed values per-origin",
        label: "Form history",
    },
    SettingDef {
        id: "sync-devices",
        category: "sync",
        keywords: "sync devices pair pairing trusted revoke device mesh trust code qr scan categories history live tabs network",
        label: "Trusted devices",
    },
    SettingDef {
        id: "advanced-restore",
        category: "advanced",
        keywords: "restore session startup crash relaunch tabs",
        label: "Restore previous session",
    },
    SettingDef {
        id: "advanced-pip",
        category: "advanced",
        keywords: "picture in picture pip video floating window auto",
        label: "Picture-in-Picture",
    },
    SettingDef {
        id: "advanced-recovery",
        category: "advanced",
        keywords: "recovery phrase vault backup mnemonic",
        label: "Vault recovery",
    },
    SettingDef {
        id: "advanced-transfer",
        category: "advanced",
        keywords: "export import settings file backup move device sync",
        label: "Move settings to another device",
    },
    SettingDef {
        id: "advanced-export",
        category: "advanced",
        keywords: "export data backup portable tar encrypted device migration download takeout",
        label: "Export my data",
    },
    SettingDef {
        id: "advanced-imports",
        category: "advanced",
        keywords: "import bookmarks history chrome firefox netscape",
        label: "Import browsing data",
    },
    SettingDef {
        id: "advanced-reset",
        category: "advanced",
        keywords: "reset defaults restore factory wipe",
        label: "Reset everything",
    },
    SettingDef {
        id: "about-version",
        category: "about",
        keywords: "about version update channel changelog",
        label: "About Ferrite",
    },
    SettingDef {
        id: "about-oss",
        category: "about",
        keywords: "open source license attribution third party phosphor",
        label: "Open-source attributions",
    },
];

/// Marker for the crate version in the settings About section.
pub const VERSION_MARKER: &str = "/*__FERRITE_VERSION__*/";

/// Assets served under `ferrite://assets/<name>`.
pub type AssetTable = std::collections::BTreeMap<String, std::sync::Arc<[u8]>>;

/// Marker inside each privileged page's `<style>` where the shell's theme
/// CSS (`ThemeConfig::theme_css`) is substituted at serve time. Pages keep
/// working with zero theme (marker left as a harmless empty comment) so
/// tests and bare handlers never depend on shell tokens.
pub const THEME_MARKER: &str = "/*__FERRITE_THEME__*/";

/// Substitute the theme CSS block for [`THEME_MARKER`]. Plain string
/// replace (no regex): an empty `css` leaves the marker comment in place.
pub fn apply_theme_css(html: &str, css: &str) -> String {
    html.replacen(THEME_MARKER, css, 1)
}

/// Minimal HTML escaping for interpolated values (`&<>"'`).
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

const PRIVILEGED_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; font-src http://ferrite.assets https://ferrite.assets; img-src 'none'; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";
/// The PDF viewer renders page bitmaps as `data:` URLs. Same lockdown as
/// privileged pages except `img-src data:`: data URLs cannot navigate,
/// script, or reach the network, so the page stays inert while showing
/// pictures. Everything else (no connect, no frames, no objects) holds.
const PDF_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; font-src http://ferrite.assets https://ferrite.assets; img-src data:; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";
const NOSNIFF: &str = "nosniff";

fn html_response(status: u16, body: Vec<u8>) -> Response<Cow<'static, [u8]>> {
    let len = body.len().to_string();
    Response::builder()
        .status(status)
        .header("Content-Type", "text/html; charset=utf-8")
        .header("Content-Security-Policy", PRIVILEGED_CSP)
        .header("X-Content-Type-Options", NOSNIFF)
        .header("Content-Length", len)
        .body(Cow::Owned(body))
        .unwrap_or_else(|_| Response::new(Cow::Borrowed(b"protocol error" as &[u8])))
}

fn pdf_response(status: u16, body: Vec<u8>) -> Response<Cow<'static, [u8]>> {
    let len = body.len().to_string();
    Response::builder()
        .status(status)
        .header("Content-Type", "text/html; charset=utf-8")
        .header("Content-Security-Policy", PDF_CSP)
        .header("X-Content-Type-Options", NOSNIFF)
        .header("Content-Length", len)
        .body(Cow::Owned(body))
        .unwrap_or_else(|_| Response::new(Cow::Borrowed(b"protocol error" as &[u8])))
}

fn asset_mime(name: &str) -> &'static str {
    if name.ends_with(".ttf") {
        "font/ttf"
    } else if name.ends_with(".otf") {
        "font/otf"
    } else if name.ends_with(".woff2") {
        "font/woff2"
    } else {
        "application/octet-stream"
    }
}

/// Handler that serves assets then delegates to the page handler.
pub fn ferrite_protocol_handler_with_assets(
    id: wry::WebViewId<'_>,
    request: Request<Vec<u8>>,
    assets: &AssetTable,
    theme_css: &str,
) -> Response<Cow<'static, [u8]>> {
    let uri = request.uri().to_string();
    // Handle both ferrite://assets/ and http(s)://ferrite.assets/ for Windows WebView2 proxy mapping
    let asset_prefixes = [
        "ferrite://assets/",
        "http://ferrite.assets/",
        "https://ferrite.assets/",
        "http://ferrite/assets/",
        "https://ferrite/assets/",
    ];
    for prefix in asset_prefixes {
        if let Some(rest) = uri.strip_prefix(prefix) {
            let name = rest.split(['?', '#']).next().unwrap_or("");
            if let Some(bytes) = assets.get(name) {
                let mime = asset_mime(name);
                let len = bytes.len().to_string();
                return Response::builder()
                    .status(200)
                    .header("Content-Type", mime)
                    .header("X-Content-Type-Options", NOSNIFF)
                    .header("Cache-Control", "public, max-age=31536000, immutable")
                    .header("Content-Length", len)
                    .body(Cow::Owned(bytes.to_vec()))
                    .unwrap_or_else(|_| Response::new(Cow::Borrowed(b"asset error" as &[u8])));
            }
            // `text/plain` does not execute, but escape anyway (info-leak hygiene).
            let safe = html_escape(name);
            return Response::builder()
                .status(404)
                .header("Content-Type", "text/plain; charset=utf-8")
                .header("X-Content-Type-Options", NOSNIFF)
                .body(Cow::Owned(format!("asset not found: {safe}").into_bytes()))
                .unwrap_or_else(|_| Response::new(Cow::Borrowed(b"" as &[u8])));
        }
    }
    // Also handle http(s)://ferrite.newtab etc via exact host match (never
    // substring: `https://evil.com/?a=ferrite&b=newtab` must not match).
    if is_ferrite_asset_uri(&uri)
        && let Some(rest) = uri
            .split_once("ferrite.assets/")
            .map(|(_, r)| r)
            .or_else(|| uri.split_once("ferrite/assets/").map(|(_, r)| r))
    {
        let name = rest.split(['?', '#']).next().unwrap_or("");
        if let Some(bytes) = assets.get(name) {
            let mime = asset_mime(name);
            let len = bytes.len().to_string();
            return Response::builder()
                .status(200)
                .header("Content-Type", mime)
                .header("X-Content-Type-Options", NOSNIFF)
                .header("Cache-Control", "public, max-age=31536000, immutable")
                .header("Content-Length", len)
                .body(Cow::Owned(bytes.to_vec()))
                .unwrap_or_else(|_| Response::new(Cow::Borrowed(b"asset error" as &[u8])));
        }
    }
    ferrite_protocol_handler(id, request, theme_css)
}

/// Handler for `ferrite://` page requests. `theme_css` (the shell's
/// `ThemeConfig::theme_css` block) is substituted for [`THEME_MARKER`] in
/// the privileged page templates; empty string leaves pages unthemed but
/// functional.
pub fn ferrite_protocol_handler(
    _id: wry::WebViewId<'_>,
    request: Request<Vec<u8>>,
    theme_css: &str,
) -> Response<Cow<'static, [u8]>> {
    let uri = request.uri().to_string();
    let themed = |html: &str| apply_theme_css(html, theme_css).into_bytes();
    match ferrite_route(&uri) {
        FerritePage::NewTab => html_response(200, themed(NEWTAB_HTML)),
        FerritePage::Crashed => html_response(200, themed(CRASHED_HTML)),
        FerritePage::BootDecision => html_response(200, themed(BOOT_HTML)),
        FerritePage::Security => html_response(200, themed(BLOCKED_HTML)),
        FerritePage::Settings => {
            let html = apply_theme_css(SETTINGS_HTML, theme_css).replacen(
                VERSION_MARKER,
                env!("CARGO_PKG_VERSION"),
                1,
            );
            html_response(200, html.into_bytes())
        }
        FerritePage::TabSearch => html_response(200, themed(TABSEARCH_HTML)),
        FerritePage::CtxMenu => html_response(200, themed(CONTEXT_HTML)),
        FerritePage::Dialog => html_response(200, themed(DIALOG_HTML)),
        FerritePage::Passwords => html_response(200, themed(PASSWORDS_HTML)),
        FerritePage::Autofill => html_response(200, themed(AUTOFILL_HTML)),
        FerritePage::About => {
            // Runtime values baked at serve time (no IPC for a static page).
            let html = apply_theme_css(ABOUT_HTML, theme_css)
                .replacen(
                    "/*__FERRITE_ABOUT_VERSION__*/",
                    env!("CARGO_PKG_VERSION"),
                    1,
                )
                .replacen(
                    "/*__FERRITE_ABOUT_ENGINE__*/",
                    &wry::webview_version().unwrap_or_else(|_| "unknown".to_owned()),
                    1,
                )
                .replacen("/*__FERRITE_ABOUT_OS__*/", std::env::consts::OS, 1);
            html_response(200, html.into_bytes())
        }
        FerritePage::Credits => html_response(200, themed(&credits_page())),
        FerritePage::TaskManager => html_response(200, themed(TASKMANAGER_HTML)),
        FerritePage::Certs => html_response(200, themed(CERTS_HTML)),
        FerritePage::ViewSource => html_response(200, themed(VIEWSOURCE_HTML)),
        FerritePage::Reader => {
            // The reader inlines the translate engine (privileged pages
            // carry no init scripts, so it cannot arrive that way). Same
            // source as the injected copy, substituted at serve time.
            let html = apply_theme_css(READER_HTML, theme_css).replacen(
                crate::translate::TRANSLATE_MARKER,
                crate::translate::translate_js(),
                1,
            );
            html_response(200, html.into_bytes())
        }
        FerritePage::Welcome => html_response(200, themed(WELCOME_HTML)),
        FerritePage::Profiles => html_response(200, themed(PROFILES_HTML)),
        FerritePage::Site => html_response(200, themed(SITE_HTML)),
        FerritePage::Pdf => pdf_response(200, themed(PDF_HTML)),
        FerritePage::Restore => html_response(200, themed(RESTORE_HTML)),
        FerritePage::Blocked => html_response(200, themed(BLOCKED_HTML)),
        FerritePage::Error => html_response(200, themed(ERROR_HTML)),
        FerritePage::History => html_response(200, themed(HISTORY_HTML)),
        FerritePage::Downloads => html_response(200, themed(DOWNLOADS_HTML)),
        FerritePage::Bookmarks => html_response(200, themed(BOOKMARKS_HTML)),
        FerritePage::Share => {
            // Server-rendered from `u` (shared URL) + `t` (title): no shell
            // push needed, so the sheet works the moment the page loads.
            // Unsharable schemes and overlong input render the honest empty
            // state (same fail-closed shape as the archive 404).
            let query = uri.split('?').nth(1).unwrap_or("");
            let raw = decode_query_param(query, "u").unwrap_or_default();
            let given_title = decode_query_param(query, "t").unwrap_or_default();
            match crate::share::share_payload(&raw, &given_title, None) {
                Some(p) => {
                    let md = crate::share::markdown_link(&p.title, &p.url);
                    let mailto = crate::share::mailto_url(&p.title, &p.url);
                    let qr = crate::share::qr_svg(&p.url).ok();
                    html_response(
                        200,
                        themed(&crate::share::render_share_page(
                            &p.url,
                            &p.title,
                            &md,
                            &mailto,
                            qr.as_deref(),
                            true,
                        )),
                    )
                }
                None => html_response(
                    200,
                    themed(&crate::share::render_share_page(
                        "(no shareable link provided)",
                        "Nothing to share",
                        "",
                        "mailto:?",
                        None,
                        false,
                    )),
                ),
            }
        }
        FerritePage::Chrome => {
            // Chrome template carries the protocol theme marker, so the
            // factory theme string (shell tokens + active pack) themes it
            // exactly like every other privileged page.
            html_response(200, themed(ferrite_chrome::tokens::CHROME_TEMPLATE))
        }
        FerritePage::Unknown => {
            let safe = html_escape(&uri);
            // Truncate absurd URIs (10KB paste) to bound the error page.
            let shown = if safe.len() > 512 {
                &safe[..512]
            } else {
                &safe
            };
            html_response(404, format!("<h1>404 - {shown}</h1>").into_bytes())
        }
        FerritePage::External => html_response(404, b"<h1>404 - not a ferrite page</h1>".to_vec()),
    }
}

/// Internal page names, shared by the router and the engine-URL
/// canonicalizer. One list so the WebView2 `http(s)://ferrite.*` mapping
/// and its reverse fold can never drift apart.
pub const BOOT_HTML: &str = r##"<!DOCTYPE html><!-- ferrite://bootdecision -->
<html><head><meta charset="utf-8"><title>Storage unavailable - Ferrite</title>
<style>
/*__FERRITE_THEME__*/
html,body{margin:0;min-height:100%;background:var(--f-bg);color:var(--f-fg);font-family:var(--f-font-ui)}
.wrap{max-width:560px;margin:0 auto;padding:64px 24px;display:flex;flex-direction:column;gap:16px}
h1{font-size:22px;margin:0}
.sub{color:var(--f-muted);font-size:13px;margin:0}
#reasons{display:flex;flex-direction:column;gap:8px}
.reason{background:var(--f-surface-1);border:1px solid var(--f-border);border-radius:var(--pk-card-radius,8px);padding:10px 14px;font-size:13px}
.row{display:flex;gap:10px;flex-wrap:wrap;margin-top:8px}
.row button{background:var(--f-surface-2);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-ctl-radius,6px);padding:10px 18px;cursor:pointer;font-size:13px;font-family:inherit}
.row button:hover{border-color:var(--f-accent)}
.row button.primary{background:var(--f-accent);border-color:var(--f-accent);color:var(--f-bg);font-weight:700}
.row button:disabled{opacity:.45;cursor:default}
.note{color:var(--f-faint);font-size:12px;margin:8px 0 0}
#recdet{margin-top:8px;border-top:1px solid var(--f-border);padding-top:12px}
#recdet summary{cursor:pointer;font-size:13px;color:var(--f-muted)}
#recphrase{width:100%;box-sizing:border-box;background:var(--f-surface-1);border:1px solid var(--f-border);color:var(--f-fg);border-radius:var(--pk-ctl-radius,6px);padding:9px 10px;font-size:13px;font-family:var(--f-font-mono)}
#recerr{color:var(--f-danger);font-size:12px;min-height:18px}
@media (prefers-reduced-motion:reduce){.row button{transition:none}}
</style></head>
<body>
<div class="wrap">
<h1>Encrypted storage is unavailable</h1>
<p class="sub">Ferrite keeps everything on this device encrypted. Its stores could not be opened, so nothing was loaded and nothing will be saved until you decide.</p>
<div id="reasons" aria-live="polite"></div>
<div class="row">
<button id="bretry" class="primary">Retry</button>
  <button id="bguest">Browse Incognito</button>
<button id="bquit">Quit</button>
</div>
<p class="note">Incognito mode runs fully in memory: history, passwords and downloads vanish when you close. Retry if you just unlocked the device keyring.</p>
<details id="recdet"><summary>Lost your device key? Recover with your 24-word phrase</summary>
<p>Paste the recovery phrase you wrote down. If it matches, the key is restored and the browser restarts.</p>
<textarea id="recphrase" rows="3" placeholder="24 words, separated by spaces"></textarea>
<p><button id="recgo">Recover and restart</button></p>
<p id="recerr" aria-live="polite"></p></details>
</div>
<script>
(function(){
  function post(o){ try{ window.ipc.postMessage(JSON.stringify(o)); }catch(e){} }
  function esc(s){ return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;'); }
  window.__ferriteBootDecision=function(d){
    d=d||{};
    var h='';
    (d.reasons||[]).forEach(function(r){ h+='<div class="reason">'+esc(r)+'</div>'; });
    if(!h) h='<div class="reason">Encrypted storage is unavailable.</div>';
    document.getElementById('reasons').innerHTML=h;
    var err=document.getElementById('recerr');
    if(d.recerr) err.textContent=d.recerr; else err.textContent='';
  };
  document.getElementById('bretry').addEventListener('click',function(){ this.disabled=true; post({type:'bootdecision',choice:'retry'}); });
  document.getElementById('bguest').addEventListener('click',function(){ this.disabled=true; post({type:'bootdecision',choice:'guest'}); });
  document.getElementById('bquit').addEventListener('click',function(){ this.disabled=true; post({type:'bootdecision',choice:'quit'}); });
  document.getElementById('recgo').addEventListener('click',function(){
    var v=document.getElementById('recphrase').value||'';
    var n=v.split(/\s+/).filter(function(w){ return w; }).length;
    var err=document.getElementById('recerr');
    if(n!==12&&n!==24){ err.textContent='Enter all 12 or 24 words.'; return; }
    err.textContent='';
    post({type:'bootrecovery',phrase:v});
  });
  post({type:'bootdecision_data'});
})();
</script>
</body></html>
"##;
pub const INTERNAL_PAGE_NAMES: &[&str] = &[
    "newtab",
    "crashed",
    "bootdecision",
    "security-warning",
    "settings",
    "tabsearch",
    "ctxmenu",
    "dialog",
    "passwords",
    "autofill",
    "about",
    "credits",
    "taskmanager",
    "certs",
    "viewsource",
    "reader",
    "welcome",
    "restore",
    "blocked",
    "error",
    "history",
    "downloads",
    "bookmarks",
    "share",
    "chrome",
    "assets",
    "profiles",
    "site",
    "pdf",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FerritePage {
    NewTab,
    Crashed,
    BootDecision,
    Security,
    Settings,
    TabSearch,
    CtxMenu,
    Dialog,
    Passwords,
    Autofill,
    About,
    Credits,
    TaskManager,
    Certs,
    ViewSource,
    Reader,
    Welcome,
    Restore,
    Profiles,
    Site,
    Pdf,
    Blocked,
    Error,
    History,
    Downloads,
    Bookmarks,
    Share,
    Chrome,
    Unknown,
    External,
}

/// Exact-match routing for privileged pages. Only `ferrite:` scheme or an
/// exact `ferrite.*` host (including the Windows WebView2
/// `http(s)://ferrite.*` mapping) routes here. Query/fragment never affect
/// the match, and substrings in path/query of external URLs never match.
fn ferrite_route(uri: &str) -> FerritePage {
    let (scheme, rest) = match uri.split_once("://") {
        Some(v) => v,
        None => return FerritePage::External,
    };
    let scheme = scheme.to_ascii_lowercase();
    // Authority is up to the first / ? #.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority
        .split('@')
        .next_back()
        .unwrap_or(authority)
        .split(':')
        .next()
        .unwrap_or(authority)
        .trim_end_matches('.')
        .to_ascii_lowercase();
    // Path is the /... portion without query/fragment, lowercased.
    let after_auth = &rest[authority.len()..];
    let path = after_auth
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let page = if path.is_empty() || path == "/" {
        host.as_str()
    } else {
        path.trim_start_matches('/').split('/').next().unwrap_or("")
    };
    // Also accept `ferrite.newtab` style hosts from the WebView2 mapping.
    let host_page = host.strip_suffix(".newtab").map(|_| "newtab").or_else(|| {
        INTERNAL_PAGE_NAMES
            .iter()
            .find(|p| host == format!("ferrite.{p}").as_str())
            .copied()
    });
    let is_internal = scheme == "ferrite"
        || host == "ferrite"
        || host.ends_with(".ferrite")
        || host == "ferrite.invalid"
        || host.ends_with(".ferrite.invalid")
        || scheme == "about" && host == "blank";
    if !is_internal {
        return FerritePage::External;
    }
    let name = host_page.unwrap_or(page);
    match name {
        "newtab" => FerritePage::NewTab,
        "crashed" => FerritePage::Crashed,
        "bootdecision" => FerritePage::BootDecision,
        "security-warning" => FerritePage::Security,
        "settings" => FerritePage::Settings,
        "tabsearch" => FerritePage::TabSearch,
        "ctxmenu" => FerritePage::CtxMenu,
        "dialog" => FerritePage::Dialog,
        "passwords" => FerritePage::Passwords,
        "autofill" => FerritePage::Autofill,
        "about" => FerritePage::About,
        "credits" => FerritePage::Credits,
        "taskmanager" => FerritePage::TaskManager,
        "certs" => FerritePage::Certs,
        "viewsource" => FerritePage::ViewSource,
        "reader" => FerritePage::Reader,
        "welcome" => FerritePage::Welcome,
        "restore" => FerritePage::Restore,
        "profiles" => FerritePage::Profiles,
        "site" => FerritePage::Site,
        "pdf" => FerritePage::Pdf,
        "blocked" => FerritePage::Blocked,
        "error" => FerritePage::Error,
        "history" => FerritePage::History,
        "downloads" => FerritePage::Downloads,
        "bookmarks" => FerritePage::Bookmarks,
        "share" => FerritePage::Share,
        "chrome" => FerritePage::Chrome,
        "assets" => FerritePage::Unknown,
        _ => FerritePage::Unknown,
    }
}

fn is_ferrite_asset_uri(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    lower.contains("://ferrite.assets/")
        || lower.contains("://ferrite/assets/")
        || lower.starts_with("ferrite://assets/")
}

/// Decode a single query parameter value (form-urlencoding: `+` → space,
/// `%XX` escapes). Returns `None` when the key is absent. Only the first
/// occurrence wins.
pub fn decode_query_param(query: &str, key: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

/// Encode a single query component value (form-urlencoding).
pub fn encode_query_component(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newtab_handler_returns_html() {
        let req = Request::builder()
            .uri("ferrite://newtab")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler("test", req, "");
        assert_eq!(resp.status(), 200);
        assert!(resp.headers().contains_key("Content-Type"));
        let body = String::from_utf8(resp.into_body().into_owned()).unwrap();
        assert!(body.contains("ferrite://newtab"));
    }

    #[test]
    fn bootdecision_handler_serves_decision_page() {
        let req = Request::builder()
            .uri("ferrite://bootdecision")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler("test", req, "");
        assert_eq!(resp.status(), 200);
        let body = String::from_utf8(resp.into_body().into_owned()).unwrap();
        assert!(body.contains("__ferriteBootDecision"), "reasons hook");
        assert!(body.contains("bootdecision_data"), "page requests reasons");
        assert!(body.contains("aria-live"), "reasons announced");
        assert!(body.contains("prefers-reduced-motion"), "motion guard");
        assert!(body.contains("bootrecovery"), "phrase recovery post");
        assert!(body.contains("recerr"), "recovery error line");
        // Workaround-host mapping lives at the factory layer (wry custom
        // protocols) plus `canonical_engine_url`, not in this handler, so
        // no http://ferrite.bootdecision assertion here.
    }

    #[test]
    fn newtab_carries_guest_banner() {
        assert!(NEWTAB_HTML.contains("id=\"guestbar\""), "banner mount");
        assert!(NEWTAB_HTML.contains("d.guest"), "hook-driven banner");
        // Private-tab parity: same promise at tab scope, own banner mount
        // that never shows alongside the window-level one.
        assert!(NEWTAB_HTML.contains("id=\"privbar\""), "private mount");
        assert!(NEWTAB_HTML.contains("d.private"), "private hook");
        assert!(
            NEWTAB_HTML.contains("d.private&&!d.guest"),
            "never both banners"
        );
    }

    #[test]
    fn settings_privacy_covers_key_and_recovery() {
        assert!(
            SETTINGS_HTML.contains("id=\"set-privacy-rotation\""),
            "rotation row"
        );
        assert!(SETTINGS_HTML.contains("postRotate"), "rotation post");
        assert!(
            SETTINGS_HTML.contains("id=\"set-privacy-recovery\""),
            "recovery row"
        );
        assert!(SETTINGS_HTML.contains("__ferriteRecovery"), "recovery hook");
        assert!(SETTINGS_HTML.contains("recPhrase"), "phrase mount");
    }

    #[test]
    fn realtime_row_names_key_requirement_and_posts_key() {
        assert!(
            SETTINGS_HTML.contains("id=\"realtimeKey\""),
            "key input exists"
        );
        assert!(
            SETTINGS_HTML.contains("realtime_key"),
            "key post type exists"
        );
        assert!(
            SETTINGS_HTML.contains("id=\"realtimeKeyState\""),
            "key state mount exists"
        );
        assert!(
            SETTINGS_HTML.contains("realtime_api_key_set"),
            "hydration reads only the set flag, never the secret"
        );
    }

    #[test]
    fn advanced_recovery_posts_working_types_only() {
        // The Advanced section used to post type:'recovery', which no
        // shell arm handles. Both buttons must post the working Privacy
        // types now; the dead posts and output sink are gone.
        assert!(
            SETTINGS_HTML.contains("postRecoveryShow()"),
            "advanced generate posts the working type"
        );
        assert!(
            SETTINGS_HTML.contains("postRecoveryDisable()"),
            "advanced disable posts the working type"
        );
        assert!(
            !SETTINGS_HTML.contains("type:'recovery'"),
            "dead recovery post fully removed"
        );
        assert!(
            !SETTINGS_HTML.contains("recoveryOut"),
            "dead output sink removed"
        );
        assert!(
            !SETTINGS_HTML.contains("__ferriteShowRecovery"),
            "dead hook removed"
        );
    }

    #[test]
    fn crashed_handler_returns_crashed_html() {
        let req = Request::builder()
            .uri("ferrite://crashed")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler("test", req, "");
        assert!(resp.into_body().windows(5).any(|w| w == b"crash"));
    }

    #[test]
    fn unknown_route_returns_404_body() {
        let req = Request::builder()
            .uri("ferrite://unknown")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler("test", req, "");
        assert_eq!(resp.status(), 404);
    }

    #[test]
    fn html_escape_neutralizes_script_markup() {
        assert_eq!(
            html_escape("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
        assert_eq!(html_escape("a&b\"c'd"), "a&amp;b&quot;c&#39;d");
    }

    #[test]
    fn reflected_xss_in_unknown_route_is_escaped() {
        // `<>"` cannot appear in a valid `http::Uri` (the HTTP layer
        // rejects them before the handler runs), so exercise the escaping
        // with URI-valid metacharacters `&` and `'`.
        let req = Request::builder()
            .uri("ferrite://unknown?x=a&y='evil")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler("test", req, "");
        assert_eq!(resp.status(), 404);
        let body = String::from_utf8(resp.into_body().into_owned()).unwrap();
        assert!(!body.contains("y='evil"));
        assert!(body.contains("&#39;evil"));
        assert!(body.contains("&amp;"));
    }

    #[test]
    fn external_url_containing_ferrite_is_not_hijacked() {
        for uri in [
            "https://evil.com/?a=ferrite&b=newtab",
            "https://ferrite.evil.com/newtab",
            "https://notferrite.example/settings",
        ] {
            let req = Request::builder().uri(uri).body(Vec::new()).unwrap();
            let resp = ferrite_protocol_handler("test", req, "");
            assert_eq!(resp.status(), 404, "uri: {uri}");
            let body = String::from_utf8(resp.into_body().into_owned()).unwrap();
            assert!(
                !body.contains("Sovereign Browser"),
                "must not serve internal page for {uri}"
            );
        }
    }

    #[test]
    fn query_codec_round_trips_ampersands() {
        let url = "https://example.com/a?b=c&d=e";
        let enc = encode_query_component(url);
        assert!(!enc.contains('&'));
        assert_eq!(
            decode_query_param(&format!("url={enc}"), "url").as_deref(),
            Some(url)
        );
        assert_eq!(
            decode_query_param("q=hello+world", "q").as_deref(),
            Some("hello world")
        );
        assert_eq!(decode_query_param("a=1&q=x", "q").as_deref(), Some("x"));
        assert_eq!(decode_query_param("a=1", "q"), None);
    }

    #[test]
    fn privileged_pages_carry_csp_and_nosniff() {
        let req = Request::builder()
            .uri("ferrite://newtab")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler("test", req, "");
        assert_eq!(resp.status(), 200);
        let csp = resp.headers().get("Content-Security-Policy").unwrap();
        assert!(csp.to_str().unwrap().contains("default-src"));
        assert_eq!(
            resp.headers().get("X-Content-Type-Options").unwrap(),
            "nosniff"
        );
    }

    fn make_assets() -> AssetTable {
        let mut t = AssetTable::new();
        t.insert(
            "geist-pixel-square.ttf".to_owned(),
            std::sync::Arc::from(&b"FAKE_TTF_BYTES"[..]),
        );
        t
    }

    #[test]
    fn asset_route_serves_registered_bytes_with_font_mime() {
        let assets = make_assets();
        let req = Request::builder()
            .uri("ferrite://assets/geist-pixel-square.ttf")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler_with_assets("test", req, &assets, "");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers().get("Content-Type").unwrap(), "font/ttf");
        assert_eq!(&*resp.into_body(), b"FAKE_TTF_BYTES");
    }

    #[test]
    fn asset_route_unknown_name_is_404() {
        let assets = make_assets();
        let req = Request::builder()
            .uri("ferrite://assets/nope.ttf")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler_with_assets("test", req, &assets, "");
        assert_eq!(resp.status(), 404);
    }

    #[test]
    fn asset_query_and_fragment_are_stripped() {
        let assets = make_assets();
        let req = Request::builder()
            .uri("ferrite://assets/geist-pixel-square.ttf?v=2#x")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler_with_assets("test", req, &assets, "");
        assert_eq!(resp.status(), 200);
    }

    #[test]
    fn non_asset_uris_fall_through_to_page_handler() {
        let assets = make_assets();
        let req = Request::builder()
            .uri("ferrite://newtab")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler_with_assets("test", req, &assets, "");
        assert_eq!(resp.status(), 200);
        let body = String::from_utf8(resp.into_body().into_owned()).unwrap();
        assert!(body.contains("ferrite://newtab"));
    }

    #[test]
    fn theme_css_replaces_marker_once() {
        let out = apply_theme_css("a/*__FERRITE_THEME__*/b/*__FERRITE_THEME__*/", ":root{}");
        assert_eq!(out, "a:root{}b/*__FERRITE_THEME__*/");
        assert_eq!(apply_theme_css("no marker", ":root{}"), "no marker");
    }

    #[test]
    fn themed_newtab_has_vars_and_no_hardcoded_colors() {
        let css = ":root{--f-bg:#090a0f}";
        let body = themed_body("ferrite://newtab", css);
        assert!(body.contains(":root{--f-bg:#090a0f}"), "injected");
        assert!(!body.contains(THEME_MARKER), "marker consumed");
        for page in [
            NEWTAB_HTML,
            CRASHED_HTML,
            BLOCKED_HTML,
            ERROR_HTML,
            SETTINGS_HTML,
            TABSEARCH_HTML,
            CONTEXT_HTML,
            DIALOG_HTML,
        ] {
            assert!(page.contains(THEME_MARKER), "marker present");
            for hard in [
                "#020203", "#E5E2E1", "#9CA3AF", "#2A2A2A", "#6366F1", "Inter,",
            ] {
                assert!(
                    !page.contains(hard),
                    "hardcoded {hard} left in privileged template"
                );
            }
        }
    }

    #[test]
    fn privileged_csp_allows_first_party_fonts_only() {
        let req = Request::builder()
            .uri("ferrite://settings")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler("test", req, "");
        let csp = resp.headers().get("Content-Security-Policy").unwrap();
        let csp = csp.to_str().unwrap();
        assert!(
            csp.contains("font-src http://ferrite.assets https://ferrite.assets"),
            "{csp}"
        );
        assert!(!csp.contains("font-src *"), "no wildcard fonts");
    }

    fn themed_body(uri: &str, css: &str) -> String {
        let assets = make_assets();
        let req = Request::builder().uri(uri).body(Vec::new()).unwrap();
        let resp = ferrite_protocol_handler_with_assets("test", req, &assets, css);
        String::from_utf8(resp.into_body().into_owned()).unwrap()
    }

    #[test]
    fn tabsearch_route_serves_palette_page() {
        let body = themed_body("ferrite://tabsearch", ":root{}");
        assert!(body.contains("Command palette"), "eyebrow");
        assert!(body.contains("__ferritePalette"), "snapshot hook");
        assert!(body.contains("__ferriteTabs"), "live refresh hook");
        assert!(body.contains("__ferriteShow"), "show hook");
        assert!(body.contains("role=\"listbox\""), "list semantics");
        assert!(body.contains("role=\"option\""), "option semantics");
        assert!(body.contains("role=\"combobox\""), "input semantics");
        assert!(body.contains("prefers-reduced-motion"), "motion guard");
    }

    #[test]
    fn share_route_renders_sheet_and_empty_state() {
        let body = themed_body(
            "ferrite://share?u=https%3A%2F%2Fexample.com%2Fa%3Futm_source%3Dx&t=Example",
            ":root{}",
        );
        // Tracking params stripped at serve time, title carried through.
        assert!(body.contains("https://example.com/a"), "clean link");
        assert!(!body.contains("utm_source"), "no tracking residue");
        assert!(body.contains("Example"), "title");
        assert!(body.contains("share_copy"), "copy post");
        assert!(body.contains("share_markdown"), "markdown post");
        assert!(body.contains("mailto:?subject=Example"), "mail link");
        assert!(body.contains("role=\"img\""), "local QR");
        assert!(!body.contains(THEME_MARKER), "marker consumed");
        // Unsharable input renders the honest empty state, never a sheet.
        let empty = themed_body("ferrite://share?u=javascript%3Aalert(1)", ":root{}");
        assert!(empty.contains("Nothing to share"), "empty state");
        assert!(!empty.contains("<button"), "no actions without a link");
    }

    #[test]
    fn ctxmenu_route_serves_context_page() {
        let body = themed_body("ferrite://ctxmenu", ":root{}");
        assert!(body.contains("__ferriteCtxMenu"), "menu hook");
        assert!(body.contains("ctx_activate"), "activation post");
        assert!(body.contains("ctx_dismiss"), "dismiss post");
        assert!(body.contains("mi.hl"), "highlight class");
        assert!(body.contains("prefers-reduced-motion"), "motion guard");
        // Overlay accessibility contract (single primitive behind menus,
        // dropdowns, dialogs): menu semantics, keyboard flow, focus home.
        assert!(body.contains("role=\"menu\""), "menu role");
        assert!(body.contains("role=\"menuitem\""), "item roles");
        assert!(body.contains("aria-label=\"Context menu\""), "menu label");
        assert!(body.contains("ArrowDown"), "arrow-key flow");
        assert!(body.contains(".focus()"), "focus-first entry");
    }

    #[test]
    fn dialog_route_serves_script_dialog_page() {
        let body = themed_body("ferrite://dialog", ":root{}");
        assert!(body.contains("__ferriteDialog"), "dialog hook");
        assert!(body.contains("dialog_respond"), "response post");
        assert!(body.contains("role=\"alertdialog\""), "modal role");
        assert!(body.contains("aria-labelledby"), "origin announced first");
        assert!(
            body.contains("Prevent this page from creating additional dialogs"),
            "suppress copy"
        );
        assert!(body.contains("prefers-reduced-motion"), "motion guard");
        // A prompt() input is page-controlled UI, never a vault field:
        // no autofill hooks may exist in this page, or the password
        // dropdown could populate a phishing-styled prompt.
        for hook in [
            "autofill",
            "__ferriteAf",
            "af_nav",
            "af_pick",
            "data-af",
            "scanner",
        ] {
            assert!(!body.contains(hook), "autofill hook left: {hook}");
        }
    }

    #[test]
    fn newtab_is_data_driven_with_rust_owned_search() {
        let body = themed_body("ferrite://newtab", ":root{}");
        assert!(body.contains("__ferriteNewTab"), "data hook");
        assert!(body.contains("ntp_pin"), "pin post");
        assert!(body.contains("ntp_unpin"), "unpin post");
        assert!(body.contains("ntp_remove"), "remove post");
        assert!(body.contains("newtab_data"), "data request");
        // Shortcut management lives behind a + tile and a dialog, not
        // always-visible raw inputs: validated http(s) before posting.
        assert!(body.contains("data-add"), "add tile");
        assert!(body.contains("Add shortcut"), "add label");
        assert!(body.contains("data-edit"), "edit button");
        assert!(body.contains("aria-modal"), "dialog semantics");
        assert!(!body.contains("id=\"cadd\""), "no raw add row");
        assert!(!body.contains("id=\"cu\""), "no raw url input");
        // No hardcoded tiles and no timer fallback: classification belongs
        // to Rust (the old setTimeout hijacked slow URL classifications).
        assert!(!body.contains("duckduckgo.com/?q="), "no search fallback");
        assert!(!body.contains("https://github.com"), "no hardcoded tiles");
        assert!(!body.contains("setTimeout"), "no race timers");
        assert!(body.contains("skip"), "skip link");
    }

    #[test]
    fn bookmarks_manager_contract_tree_table_query() {
        let body = themed_body("ferrite://bookmarks", ":root{}");
        assert!(body.contains("__ferriteBmTree"), "tree hook");
        assert!(body.contains("__ferriteBmChildren"), "children hook");
        assert!(body.contains("__ferriteBmDups"), "dups hook");
        assert!(body.contains("role=\"tree\""), "tree pattern");
        assert!(body.contains("aria-expanded"), "folder state");
        assert!(body.contains("bookmarks_move"), "move post");
        assert!(body.contains("bookmarks_rename"), "rename post");
        assert!(body.contains("bookmarks_delete_many"), "bulk delete post");
        assert!(body.contains("bookmarks_commit"), "enter commits nav");
        assert!(body.contains("get('folder')"), "?folder preset");
        assert!(body.contains("get('highlight')"), "?highlight preset");
    }

    #[test]
    fn chrome_route_serves_shell_with_renderer_hook() {
        let body = themed_body("ferrite://chrome", ":root{}");
        assert!(body.contains("__ferriteChrome"), "renderer hook");
        assert!(body.contains("__ferriteTheme"), "pack-switch hook");
        assert!(body.contains("request_state"), "bootstraps from shell");
        assert!(!body.contains(THEME_MARKER), "marker consumed");
        assert!(!body.contains("handled via egui"), "no stale shell refs");
    }

    #[test]
    fn search_route_is_404_unknown() {
        let req = Request::builder()
            .uri("ferrite://search")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler("test", req, ":root{}");
        assert_eq!(resp.status(), 404);
        let body = String::from_utf8(resp.into_body().into_owned()).unwrap();
        assert!(body.contains("404"), "unknown route");
        assert!(!body.contains("chrome shell"), "no stale stub");
    }

    #[test]
    fn list_pages_carry_hooks_markers_and_no_hardcoded_colors() {
        for (uri, hook) in [
            ("ferrite://history", "__ferriteHistory"),
            ("ferrite://downloads", "__ferriteDownloads"),
            ("ferrite://bookmarks", "__ferriteBmTree"),
        ] {
            let body = themed_body(uri, ":root{}");
            assert!(body.contains(hook), "hook {uri}");
            assert!(body.contains("textContent"), "no innerHTML rows {uri}");
            assert!(!body.contains(THEME_MARKER), "marker consumed {uri}");
            assert!(
                body.contains("prefers-reduced-motion"),
                "motion guard {uri}"
            );
        }
        assert!(
            themed_body("ferrite://downloads", ":root{}").contains("No downloads yet"),
            "downloads empty state"
        );
        assert!(
            themed_body("ferrite://history", ":root{}").contains("No history yet"),
            "history empty state"
        );
        assert!(
            themed_body("ferrite://bookmarks", ":root{}").contains("save pages with Ctrl+D"),
            "bookmarks empty state"
        );
    }

    #[test]
    fn history_page_honors_query_param_contract() {
        let body = themed_body("ferrite://history", ":root{}");
        assert!(body.contains("URLSearchParams"), "param parsing");
        assert!(body.contains("sp.get('q')"), "?q preset");
        assert!(body.contains("sp.get('site')"), "?site preset");
        assert!(body.contains("replaceState"), "typing swaps state");
        assert!(body.contains("history_commit"), "enter commits nav");
        // Downloads/bookmarks share the macro but stay unparameterized.
        let dl = themed_body("ferrite://downloads", ":root{}");
        assert!(!dl.contains("history_commit"), "no commit outside history");
    }

    #[test]
    fn downloads_page_contract_keyed_live_announced() {
        let body = themed_body("ferrite://downloads", ":root{}");
        assert!(body.contains("__ferriteDownloads"), "rows hook");
        assert!(body.contains("URLSearchParams"), "param parsing");
        assert!(body.contains("downloads_commit"), "enter commits nav");
        assert!(body.contains("replaceState"), "typing swaps state");
        assert!(body.contains("__dlRows"), "keyed rows");
        assert!(body.contains("__dlFlash"), "?id highlight");
        assert!(body.contains("sr-live"), "completion announcer");
        assert!(body.contains("aria-live"), "live region");
        // Percentages never announce; only terminal transitions do.
        assert!(!body.contains("aria-valuenow"), "no progress semantics");
    }

    #[test]
    fn settings_catalogue_matches_template_controls() {
        // Every catalogue entry must resolve to a real control anchor and
        // carry searchable keywords; categories must exist as nav sections.
        for def in SETTINGS_CATALOGUE {
            let anchor = format!("id=\"set-{}\"", def.id);
            assert!(
                SETTINGS_HTML.contains(&anchor),
                "catalogue {} has no control anchor",
                def.id
            );
            assert!(
                !def.keywords.trim().is_empty(),
                "catalogue {} has no keywords",
                def.id
            );
            let section = format!("id=\"cat-{}\"", def.category);
            assert!(
                SETTINGS_HTML.contains(&section),
                "category {} has no section",
                def.category
            );
            for word in def.keywords.split_whitespace() {
                assert!(
                    SETTINGS_HTML.to_lowercase().contains(word),
                    "keyword {word} of {} missing from page",
                    def.id
                );
            }
            assert!(
                !def.label.trim().is_empty(),
                "catalogue {} has no display label",
                def.id
            );
            assert!(
                SETTINGS_HTML.contains(def.label),
                "label of {} missing from page",
                def.id
            );
        }
        // Served page: theme + version markers consumed, search box present.
        let body = themed_body("ferrite://settings", ":root{}");
        assert!(!body.contains(THEME_MARKER), "theme marker consumed");
        assert!(!body.contains(VERSION_MARKER), "version marker consumed");
        assert!(body.contains(env!("CARGO_PKG_VERSION")), "version shown");
        assert!(body.contains("id=\"setsearch\""), "search box");
        assert!(body.contains("__ferriteSetSettings"), "value push hook");
    }

    #[test]
    fn language_section_posts_spellcheck_controls() {
        let body = themed_body("ferrite://settings", ":root{}");
        assert!(
            body.contains("id=\"set-language-spellcheck\""),
            "toggle anchor"
        );
        assert!(body.contains("id=\"set-language-langs\""), "langs anchor");
        assert!(body.contains("post('spellcheck'"), "toggle post");
        assert!(body.contains("spellcheck_langs"), "langs post + push");
        assert!(body.contains("id=\"cat-language\""), "nav section");
        assert!(
            body.contains("id=\"set-language-translate\""),
            "translate anchor"
        );
        assert!(
            body.contains("id=\"set-language-translate-target\""),
            "target anchor"
        );
        assert!(
            body.contains("post('translate_enabled'"),
            "translate toggle post"
        );
        assert!(body.contains("translate_target"), "target post + push");
    }

    #[test]
    fn exclusions_row_posts_and_receives() {
        let body = themed_body("ferrite://settings", ":root{}");
        assert!(body.contains("id=\"set-privacy-exclusions\""), "row anchor");
        assert!(body.contains("__ferriteExclusions"), "list hook");
        assert!(body.contains("exclusion_add"), "add post");
        assert!(body.contains("exclusion_remove"), "remove post");
        assert!(body.contains("exclusions_query"), "load request");
    }

    #[test]
    fn perm_grants_row_posts_and_receives() {
        let body = themed_body("ferrite://settings", ":root{}");
        assert!(body.contains("id=\"set-privacy-perms\""), "row anchor");
        assert!(body.contains("__ferritePermGrants"), "list hook");
        assert!(body.contains("perm_grant_revoke"), "revoke post");
        assert!(body.contains("perm_grants_query"), "load request");
    }

    #[test]
    fn sync_section_posts_device_controls() {
        let body = themed_body("ferrite://settings", ":root{}");
        assert!(body.contains("id=\"set-sync-devices\""), "row anchor");
        assert!(body.contains("id=\"cat-sync\""), "nav section");
        assert!(body.contains("__ferriteSyncDevices"), "devices hook");
        assert!(body.contains("__ferriteSyncCode"), "code hook");
        assert!(body.contains("sync_devices"), "load request");
        assert!(body.contains("sync_code_show"), "code post");
        assert!(body.contains("sync_pair_submit"), "pair post");
        assert!(body.contains("sync_pair_approve"), "approve post");
        assert!(body.contains("sync_pair_deny"), "deny post");
        assert!(body.contains("sync_trust_revoke"), "revoke post");
        assert!(body.contains("sync_category"), "category post");
        assert!(body.contains("sync_open_tab"), "live-tab open post");
        assert!(body.contains("id=\"syncCats\""), "categories mount");
        assert!(body.contains("id=\"syncPeers\""), "peers mount");
        assert!(body.contains("id=\"syncLive\""), "live tabs mount");
        assert!(body.contains("id=\"syncStatus\""), "status mount");
        assert!(body.contains("id=\"set-sync-relay\""), "relay row anchor");
        assert!(body.contains("id=\"relayUrls\""), "relay input");
        assert!(body.contains("id=\"relayState\""), "relay state mount");
        assert!(body.contains("sync_relay_save"), "relay save post");
        assert!(body.contains("sync_relay_clear"), "relay clear post");
        assert!(body.contains("id=\"syncQr\""), "qr mount");
        assert!(body.contains("p.qr"), "qr render read");
        assert!(
            body.contains("id=\"syncCodeLeft\""),
            "expiry countdown mount"
        );
        assert!(body.contains("expires_at_ms"), "expiry render read");
        assert!(body.contains("catching_up"), "catch-up render read");
        assert!(body.contains("Syncing"), "catch-up copy");
        assert!(body.contains("last_seen_ms"), "last-seen render read");
        assert!(
            body.contains("store-and-forward"),
            "relay copy states the mailbox contract"
        );
    }

    #[test]
    fn notifications_kill_switch_posts_and_reads() {
        let body = themed_body("ferrite://settings", ":root{}");
        assert!(body.contains("id=\"notifGlobal\""), "toggle anchor");
        assert!(body.contains("post('notifications'"), "toggle post");
        assert!(body.contains("s.notifications_global"), "envelope read");
    }

    #[test]
    fn private_engine_select_posts_and_reads() {
        let body = themed_body("ferrite://settings", ":root{}");
        assert!(body.contains("id=\"enginePrivate\""), "private select");
        assert!(body.contains("select_private_engine"), "private post");
        assert!(body.contains("s.private_engine"), "envelope read");
    }

    #[test]
    fn vertical_tabs_toggle_posts_and_reads() {
        let body = themed_body("ferrite://settings", ":root{}");
        assert!(body.contains("id=\"tabsVertical\""), "toggle anchor");
        assert!(body.contains("post('tabs_vertical'"), "toggle post");
        assert!(body.contains("s.tabs_vertical"), "envelope read");
    }

    #[test]
    fn remote_suggestions_toggle_posts() {
        let body = themed_body("ferrite://settings", ":root{}");
        assert!(body.contains("id=\"suggestions_remote\""), "toggle anchor");
        assert!(body.contains("postSugRemote"), "toggle post");
        assert!(body.contains("s.suggestions_remote"), "envelope read");
        assert!(
            body.contains("Remote suggestions (DuckDuckGo)"),
            "honest scope: non-DDG users know why no rows appear"
        );
    }

    #[test]
    fn ask_downloads_toggle_posts() {
        let body = themed_body("ferrite://settings", ":root{}");
        assert!(body.contains("id=\"askDownloads\""), "toggle anchor");
        assert!(body.contains("postAskDownloads"), "toggle post");
        assert!(body.contains("s.ask_downloads"), "envelope read");
    }

    #[test]
    fn history_retention_posts_and_reads() {
        let body = themed_body("ferrite://settings", ":root{}");
        assert!(body.contains("id=\"set-privacy-retention\""), "row anchor");
        assert!(body.contains("id=\"historyRetention\""), "select anchor");
        assert!(body.contains("postHistoryRetention"), "select post");
        assert!(body.contains("s.history_retention_days"), "envelope read");
    }

    #[test]
    fn about_update_banner_reads_query() {
        let body = themed_body("ferrite://about", ":root{}");
        assert!(body.contains("id=\"updated\""), "banner mount");
        assert!(body.contains("updated"), "query read");
        assert!(body.contains("just updated from version"), "banner copy");
        assert!(body.contains("id=\"updateRestart\""), "restart button");
        assert!(body.contains("update_restart"), "restart post");
    }

    #[test]
    fn about_page_carries_theme_aware_banner() {
        let body = themed_body("ferrite://about", ":root{}");
        assert!(
            body.contains(r#"<div class="brandbanner" aria-hidden="true">"#),
            "masthead mount"
        );
        assert!(body.contains("mark-dark"), "dark banner");
        assert!(body.contains("mark-light"), "light banner");
        // Brand banners carry fixed brand colors by design (not theme
        // vars), but the markup itself stays inert.
        for svg in [
            include_str!("../../../brand/dark_banner.svg"),
            include_str!("../../../brand/light_banner.svg"),
        ] {
            for bad in [
                "<script",
                "onclick",
                "onload",
                "onerror",
                "foreignObject",
                "https://",
                "href=",
            ] {
                assert!(!svg.contains(bad), "inert svg: {bad}");
            }
            assert!(
                svg.matches("http").count() == 1
                    && svg.contains("xmlns=\"http://www.w3.org/2000/svg\""),
                "only the svg namespace references http"
            );
        }
    }

    #[test]
    fn profiles_picker_serves_and_lists() {
        let body = themed_body("ferrite://profiles", ":root{}");
        assert!(body.contains("Who is browsing?"), "picker copy");
        assert!(body.contains("__ferriteProfiles"), "list hook");
        assert!(body.contains("profiles_query"), "load request");
        assert!(body.contains("profile_pick"), "pick post");
        assert!(body.contains("profile_default"), "default post");
        assert!(body.contains("profile_incognito"), "incognito post");
        assert!(body.contains("profile_quit"), "quit post");
        assert!(body.contains("profile_picker"), "ask-always post");
        assert!(INTERNAL_PAGE_NAMES.contains(&"profiles"), "privileged page");
    }

    #[test]
    fn site_page_serves_with_query_and_forget() {
        let body = themed_body("ferrite://site", ":root{}");
        assert!(body.contains("__ferriteSite"), "data hook");
        assert!(body.contains("site_query"), "load request");
        assert!(body.contains("site_forget"), "forget post");
        assert!(body.contains("confirm("), "explicit confirm");
        assert!(body.contains("origin"), "origin param read");
        assert!(INTERNAL_PAGE_NAMES.contains(&"site"), "privileged page");
    }

    #[test]
    fn pdf_page_serves_with_doc_param_and_hooks() {
        let body = themed_body("ferrite://pdf/?doc=abc", ":root{}");
        assert!(body.contains("__ferritePdfDoc"), "doc hook");
        assert!(body.contains("__ferritePdfPage"), "page hook");
        assert!(body.contains("__ferritePdfSearch"), "search hook");
        assert!(body.contains("__ferritePdfForms"), "forms hook");
        assert!(body.contains("__ferritePdfSaved"), "saved hook");
        assert!(body.contains("__ferritePdfError"), "error hook");
        assert!(body.contains("__ferritePdfThumb"), "thumb hook");
        assert!(body.contains("pdf_doc"), "doc post");
        assert!(body.contains("pdf_unlock"), "unlock post");
        assert!(INTERNAL_PAGE_NAMES.contains(&"pdf"), "privileged page");
    }

    #[test]
    fn pdf_page_csp_allows_data_images_only() {
        let req = http::Request::builder()
            .uri("ferrite://pdf/?doc=abc")
            .body(Vec::new())
            .unwrap();
        let resp = ferrite_protocol_handler("test", req, ":root{}");
        let csp = resp
            .headers()
            .get("Content-Security-Policy")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(csp.contains("img-src data:"), "page bitmaps allowed");
        assert!(!csp.contains("img-src 'none'"), "no blanket image ban");
        for tight in [
            "connect-src 'none'",
            "object-src 'none'",
            "frame-src 'none'",
            "form-action 'none'",
        ] {
            assert!(csp.contains(tight), "lockdown holds: {tight}");
        }
    }

    #[test]
    fn pdf_page_is_offline_safe() {
        // No remote references anywhere: the viewer must render with
        // zero network (the CSP would block it anyway; this pins the
        // source so a future edit cannot even try).
        for bad in [
            "https://",
            "http://",
            "<link",
            "@import",
            "fetch(",
            "XMLHttpRequest",
            "innerHTML",
        ] {
            assert!(!PDF_HTML.contains(bad), "offline-safe: {bad}");
        }
        // All document-sourced strings render via textContent (the one
        // innerHTML in the codebase idiom is absent here by design).
        assert!(PDF_HTML.contains("textContent"), "safe DOM writes");
    }

    #[test]
    fn error_page_carries_vendored_illustration() {
        let body = themed_body("ferrite://error", ":root{}");
        assert!(
            body.contains(r#"<div class="art" aria-hidden="true"><svg"#),
            "art mount"
        );
        assert!(
            body.contains("aria-label=\"person no signal\""),
            "koboyo figure"
        );
        assert!(
            body.contains(r#"<span class="badge"><svg"#)
                && body.contains("aria-label=\"wifi off\""),
            "no-internet badge"
        );
        // Every vendored SVG stays inert: no scripts, handlers, foreign
        // objects, or external references (offline-safe by construction,
        // not by review). The single `http` occurrence per file is the
        // mandatory svg namespace, which triggers no fetch.
        for svg in [
            include_str!("../assets/person-no-signal.svg"),
            include_str!("../assets/wifi-off.svg"),
        ] {
            for bad in [
                "<script",
                "onclick",
                "onload",
                "onerror",
                "foreignObject",
                "https://",
                "href=",
                "#",
            ] {
                assert!(!svg.contains(bad), "inert svg: {bad}");
            }
            assert!(
                svg.matches("http").count() == 1
                    && svg.contains("xmlns=\"http://www.w3.org/2000/svg\""),
                "only the svg namespace references http"
            );
        }
    }

    #[test]
    fn welcome_steps_carry_vendored_illustrations() {
        let body = themed_body("ferrite://welcome", ":root{}");
        assert_eq!(
            body.matches("class=\"tart").count(),
            8,
            "one illustration per tour step"
        );
        // Step 0 carries the Ferrite mark itself (theme-aware pair);
        // the rest carry distinct Koboyo assets, not one repeated.
        assert!(body.contains("mark-dark"), "dark brand mark");
        assert!(body.contains("mark-light"), "light brand mark");
        assert!(body.contains("#D9601F"), "brand orange inlined");
        for label in [
            "suitcase",
            "magnifying glass",
            "aria-label=\"shield check\"",
            "person picking theme",
            "sync",
            "house",
            "person launching rocket",
        ] {
            assert!(body.contains(label), "tour art: {label}");
        }
        assert_eq!(
            body.matches("aria-label=\"shield check\"").count(),
            1,
            "shield-check lives on the protection step only"
        );
        // Same inertness bar as the error page art. Brand art carries
        // fixed brand colors, so `#` is exempt there only.
        for svg in [
            include_str!("../assets/suitcase.svg"),
            include_str!("../assets/magnifying-glass.svg"),
            include_str!("../assets/shield-check.svg"),
            include_str!("../assets/person-picking-theme.svg"),
            include_str!("../assets/sync.svg"),
            include_str!("../assets/house.svg"),
            include_str!("../assets/person-launching-rocket.svg"),
            include_str!("../../../brand/icon_on_dark.svg"),
            include_str!("../../../brand/icon_on_light.svg"),
        ] {
            for bad in [
                "<script",
                "onclick",
                "onload",
                "onerror",
                "foreignObject",
                "https://",
                "href=",
            ] {
                assert!(!svg.contains(bad), "inert svg: {bad}");
            }
            assert!(
                svg.matches("http").count() == 1
                    && svg.contains("xmlns=\"http://www.w3.org/2000/svg\""),
                "only the svg namespace references http"
            );
        }
        // Koboyo art stays theme-tinted (no fixed colors); brand art is
        // the deliberate exception.
        for svg in [
            include_str!("../assets/suitcase.svg"),
            include_str!("../assets/magnifying-glass.svg"),
            include_str!("../assets/shield-check.svg"),
            include_str!("../assets/person-picking-theme.svg"),
            include_str!("../assets/sync.svg"),
            include_str!("../assets/house.svg"),
            include_str!("../assets/person-launching-rocket.svg"),
        ] {
            assert!(!svg.contains('#'), "koboyo art stays currentColor");
        }
    }

    #[test]
    fn welcome_tour_has_eight_steps_with_theme_and_sync() {
        let body = themed_body("ferrite://welcome", ":root{}");
        for step in 0..8 {
            assert!(
                body.contains(&format!("data-step=\"{step}\"")),
                "step {step} present"
            );
        }
        assert!(body.contains("id=\"packsel\""), "theme select");
        assert!(body.contains("select_pack"), "theme post");
        assert!(body.contains("ferrite://settings#sync"), "sync deep link");
        assert!(body.contains("var step=0,last=7;"), "step machine");
    }

    #[test]
    fn startup_section_posts_and_reads() {
        let body = themed_body("ferrite://settings", ":root{}");
        assert!(body.contains("id=\"set-startup-mode\""), "mode row");
        assert!(body.contains("id=\"startupMode\""), "mode select");
        assert!(body.contains("postStartupMode"), "mode post");
        assert!(body.contains("s.startup_mode"), "mode read");
        assert!(body.contains("id=\"set-startup-urls\""), "urls row");
        assert!(body.contains("__ferriteStartupUrls"), "list hook");
        assert!(body.contains("startupUrlAdd"), "add post");
        assert!(body.contains("s.startup_urls"), "list read");
    }

    #[test]
    fn sleep_settings_post_and_read() {
        let body = themed_body("ferrite://settings", ":root{}");
        assert!(body.contains("id=\"set-performance-sleep\""), "timeout row");
        assert!(body.contains("id=\"sleepTimeout\""), "timeout select");
        assert!(body.contains("postSleepTimeout"), "timeout post");
        assert!(body.contains("s.sleep_timeout_min"), "timeout read");
        assert!(
            body.contains("id=\"set-performance-exceptions\""),
            "exceptions row"
        );
        assert!(body.contains("__ferriteSleepExceptions"), "list hook");
        assert!(body.contains("sleepExceptionAdd"), "add post");
        assert!(body.contains("s.sleep_exceptions"), "list read");
    }

    #[test]
    fn export_my_data_posts_all_flows() {
        let body = themed_body("ferrite://settings", ":root{}");
        assert!(body.contains("id=\"set-advanced-export\""), "row anchor");
        assert!(body.contains("exportAllArm"), "tar two-step arm");
        assert!(body.contains("type:'export_all'"), "tar post");
        assert!(body.contains("type:'backup_export'"), "backup post");
        assert!(body.contains("type:'backup_import'"), "restore pick post");
        assert!(body.contains("type:'backup_restore'"), "restore post");
        assert!(body.contains("__ferriteBackupPrompt"), "prompt hook");
        assert!(body.contains("plaintext"), "plaintext cost named");
        assert!(body.contains("exportHistory"), "history export button");
        assert!(
            body.contains("type:'import',kind:'export_history'"),
            "history export post"
        );
        assert!(
            body.contains("Import Bookmarks (HTML, JSON)"),
            "bookmark formats named"
        );
    }

    #[test]
    fn passwords_export_arms_confirm() {
        let body = themed_body("ferrite://passwords", ":root{}");
        assert!(body.contains("id=\"exp\""), "export button");
        assert!(body.contains("passwords_export"), "export post");
        assert!(body.contains("plaintext"), "honest cost copy");
        assert!(body.contains("__ferritePasswordConflicts"), "conflict hook");
        assert!(body.contains("data-keep"), "keep button");
        assert!(body.contains("data-use"), "use-imported button");
        assert!(body.contains("passwords_conflict_resolve"), "resolve post");
    }

    #[test]
    fn welcome_route_serves_stepper() {
        let body = themed_body("ferrite://welcome", ":root{}");
        assert!(body.contains("__ferriteWelcomeDetect"), "detect hook");
        assert!(body.contains("__ferriteWelcomeResult"), "result hook");
        assert!(
            body.contains("__ferriteSetSettings"),
            "shared settings push"
        );
        assert!(body.contains("welcome_done"), "done post");
        assert!(body.contains("welcome_import"), "import post");
        assert!(body.contains("welcome_passwords"), "passwords post");
        assert!(body.contains("welcome_default_browser"), "default post");
        assert!(body.contains("Step "), "progress readout");
        assert!(body.contains("aria-live"), "announced progress");
        assert!(body.contains("r.skipped"), "skipped count read");
        assert!(body.contains("r.conflicts"), "conflict count read");
        assert!(body.contains("r.note"), "caveat read");
    }

    #[test]
    fn restore_route_serves_decider() {
        let body = themed_body("ferrite://restore", ":root{}");
        assert!(body.contains("__ferriteRestore"), "data hook");
        assert!(body.contains("restore_data"), "data request");
        assert!(body.contains("restore_selected"), "subset post");
        assert!(body.contains("restore_all"), "all post");
        assert!(body.contains("restore_fresh"), "fresh post");
        assert!(body.contains("restore_diag"), "diagnostics post");
        // Previous-version rollback: hidden unless the updater retained
        // one, confirmed before posting (no dead button).
        assert!(body.contains("restore_rollback"), "rollback post");
        assert!(body.contains("can_rollback"), "retention gate");
        assert!(body.contains("was active"), "culprit hint");
        assert!(body.contains("role=\"group\""), "labeled list");
    }

    #[test]
    fn autofill_route_serves_manager() {
        let body = themed_body("ferrite://autofill", ":root{}");
        assert!(body.contains("__ferriteAutofill"), "list hook");
        assert!(body.contains("__ferriteAutofillError"), "error hook");
        assert!(body.contains("autofill_add_address"), "address post");
        assert!(body.contains("autofill_add_card"), "card post");
        assert!(body.contains("autofill_copy_card"), "card copy post");
        assert!(body.contains("data-ccopy"), "card copy button");
        assert!(body.contains("autofill_delete"), "delete post");
        assert!(body.contains("autofill_query"), "load request");
        assert!(body.contains("get('section')"), "?section preset");
        assert!(body.contains("get('highlight')"), "?highlight preset");
        // No CVV input or storage key anywhere (the guarantee copy names
        // CVV deliberately, that sentence is the point, not a surface).
        assert!(!body.contains("data-k=\"cvv\""), "no cvv input");
        assert!(!body.contains("data-k=\"cvc\""), "no cvc input");
        assert!(!body.contains("cc-csc"), "no csc fill path");
    }

    #[test]
    fn blocked_route_serves_interstitial() {
        let body = themed_body("ferrite://blocked?host=evil.test&at=1", ":root{}");
        assert!(body.contains("may harm your computer"), "warning copy");
        assert!(body.contains("get('host')"), "host preset");
        assert!(body.contains("history.back()"), "go-back path");
        assert!(body.contains("ferrite://newtab"), "safe exit");
        // Calibrated interstitial: detail query + trust/report actions.
        assert!(body.contains("__ferriteBlocked"), "detail hook");
        assert!(body.contains("blocked_query"), "detail request");
        assert!(body.contains("threat_trust"), "override action");
        assert!(body.contains("threat_report"), "report action");
        // No free navigation past the warning: the only hrefs are back
        // and the safe exit (trust/report go through typed IPC).
        assert!(!body.contains("location.href='http"), "no http exit");
    }

    #[test]
    fn error_route_serves_interstitial() {
        let body = themed_body(
            "ferrite://error?url=https%3A%2F%2Fa.test%2F&reason=timeout",
            ":root{}",
        );
        assert!(body.contains("get('url')"), "url preset");
        assert!(body.contains("get('reason')"), "reason preset");
        assert!(body.contains("history.back()"), "go-back path");
        assert!(body.contains("ferrite://newtab"), "safe exit");
        assert!(!body.contains("postMessage"), "no IPC surface");
    }

    #[test]
    fn about_route_serves_static_document() {
        let body = themed_body("ferrite://about", ":root{}");
        assert!(body.contains("id=\"version\""), "version anchor");
        assert!(body.contains("id=\"pages\""), "index anchor");
        assert!(body.contains("id=\"legal\""), "legal anchor");
        assert!(body.contains("ferrite://settings"), "index links settings");
        assert!(
            body.contains("ferrite://passwords"),
            "index links passwords"
        );
        assert!(body.contains("about_copy"), "diagnostic post");
        // Update loop surface: status region, manual check, state hook.
        // Progress and restart copy arrive via the hook; the page never
        // invents states the backend did not report.
        assert!(body.contains("id=\"updates\""), "updates anchor");
        assert!(body.contains("role=\"status\""), "status region");
        assert!(body.contains("update_check"), "manual check post");
        assert!(body.contains("update_query"), "state query post");
        assert!(body.contains("__ferriteUpdate"), "state hook");
        // The copy receipt is the chrome toast (role=status,
        // aria-live=polite). The page posts and stays silent instead of
        // an optimistic inline label that could not know the real result.
        assert!(!body.contains("Copied to clipboard"), "no optimistic label");
        // Runtime values baked (no placeholders survive serving).
        assert!(!body.contains("FERRITE_ABOUT_VERSION"), "version baked");
        assert!(!body.contains("FERRITE_ABOUT_ENGINE"), "engine baked");
        assert!(!body.contains("FERRITE_ABOUT_OS"), "os baked");
        assert!(body.contains(env!("CARGO_PKG_VERSION")), "real version");
        assert!(body.contains("LinkId=2124703"), "runtime update link");
        // Crash-reporting boundary, stated where users can find it.
        assert!(body.contains("ferrite-crash.log"), "local crash log named");
        assert!(
            body.contains("Diagnostics &amp; feedback"),
            "OS setting named"
        );
    }

    #[test]
    fn taskmanager_route_serves_tables() {
        let body = themed_body("ferrite://taskmanager", ":root{}");
        assert!(body.contains("__ferriteTaskman"), "snapshot hook");
        assert!(body.contains("taskman_query"), "load request");
        assert!(body.contains("taskman_end_task"), "end task post");
        assert!(body.contains("taskman_close"), "close post");
        assert!(body.contains("taskman_kill"), "kill post");
        assert!(body.contains("aria-live"), "ended announcer");
        assert!(body.contains("data-sort"), "column sort");
    }

    #[test]
    fn certs_route_serves_viewer() {
        let body = themed_body("ferrite://certs", ":root{}");
        assert!(body.contains("__ferriteCerts"), "report hook");
        assert!(body.contains("certs_query"), "load request");
        assert!(body.contains("SHA-256"), "fingerprint row");
        assert!(body.contains("SANs"), "san list");
        assert!(body.contains("san_suspicious"), "homograph flag");
        assert!(body.contains("<details"), "native chain disclosure");
        assert!(body.contains("role=\"status\""), "verdict region");
        assert!(body.contains("certs_trust"), "trust action");
        assert!(body.contains("certs_revoke"), "revoke action");
        assert!(
            body.contains("update-ca-certificates"),
            "os import guidance"
        );
    }

    #[test]
    fn viewsource_route_serves_escaped_viewer() {
        let body = themed_body("ferrite://viewsource", ":root{}");
        assert!(body.contains("__ferriteViewSource"), "report hook");
        assert!(body.contains("viewsource_query"), "load request");
        assert!(body.contains("viewsource_wrap"), "wrap pref");
        assert!(body.contains("counter-reset"), "css line numbers");
        assert!(body.contains("role=\"document\""), "document region");
        // No executable surface: rows built from pre-escaped text only.
        assert!(!body.contains("innerHTML=d.escaped"), "no raw insert");
    }

    #[test]
    fn reader_route_serves_sanitized_page() {
        let body = themed_body("ferrite://reader", ":root{}");
        assert!(body.contains("__ferriteReader"), "article hook");
        assert!(body.contains("reader_query"), "load request");
        assert!(body.contains("reader_pref"), "pref posts");
        assert!(body.contains("role=\"toolbar\""), "options toolbar");
        assert!(body.contains("aria-pressed"), "toggle state");
        assert!(body.contains("id=\"translatebtn\""), "translate button");
        assert!(body.contains("__ferriteTranslate"), "inlined engine");
        assert!(
            !body.contains(crate::translate::TRANSLATE_MARKER),
            "engine marker consumed"
        );
        // innerHTML appears exactly once as an assignment: the DOMPurify-clean
        // body, everything else is textContent.
        assert_eq!(
            body.matches(".innerHTML=").count(),
            1,
            "single innerHTML surface"
        );
    }

    #[test]
    fn reader_extract_bundles_libs_with_shim() {
        let js = reader_extract_js();
        assert!(js.contains("module={exports:{}}"), "cjs shim");
        assert!(js.contains("__ferriteReadability"), "install guard");
        assert!(js.contains("DOMPurify"), "sanitizer bundled");
        assert!(js.contains("reader_chunk"), "chunked post");
        assert!(js.contains("reader_result"), "result post");
    }

    #[test]
    fn credits_page_groups_licenses_with_crates() {
        let body = themed_body("ferrite://credits", ":root{}");
        assert!(body.contains("id=\"groups\""), "group container");
        assert!(body.contains("MIT"), "mit group");
        assert!(body.contains("Apache-2.0"), "apache group");
        assert!(body.contains("wry"), "major dep listed");
        assert!(body.contains("tokio"), "transitive dep listed");
        assert!(body.contains("Phosphor Icons"), "manual assets");
        assert!(body.contains("WebView2"), "manual engine terms");
        assert!(!body.contains("FERRITE_CREDITS"), "groups baked");
    }

    #[test]
    fn credits_snapshot_covers_lock() {
        // Drift gate (mirrors the CI regenerate-diff): every locked package
        // must appear in the committed snapshot, or be a classified
        // non-shipped crate below. A new miss fails with its name. Either
        // regenerate (it ships) or classify it here (it doesn't).
        // Non-shipped = target-gated away, inactive-optional, or dev-only:
        // none compile into the host binary cargo-about resolves.
        const NONSHIPPED: &[(&str, &str)] = &[
            ("android_system_properties", "android-target"),
            ("console_error_panic_hook", "wasm-target"),
            ("console_log", "wasm-target"),
            ("iana-time-zone-haiku", "haiku-target"),
            ("objc2-cloud-kit", "apple-target"),
            ("objc2-contacts", "apple-target"),
            ("objc2-core-data", "apple-target"),
            ("objc2-core-image", "apple-target"),
            ("objc2-core-location", "apple-target"),
            ("objc2-io-surface", "apple-target"),
            ("objc2-link-presentation", "apple-target"),
            ("objc2-metal", "apple-target"),
            ("objc2-quartz-core", "apple-target"),
            ("objc2-symbols", "apple-target"),
            ("objc2-uniform-type-identifiers", "apple-target"),
            ("objc2-user-notifications", "apple-target"),
            ("openssl-src", "inactive-optional:sqlcipher"),
            ("openssl-sys", "inactive-optional:sqlcipher"),
            ("pdfium-render", "inactive-optional:pdfium"),
            ("piston-float", "inactive-optional:pdfium-via-pdfium-render"),
            ("vecmath", "inactive-optional:pdfium-via-pdfium-render"),
            ("byteorder", "inactive-optional"),
            ("chrono", "inactive-optional"),
            ("iana-time-zone", "inactive-optional"),
            ("cookie_store", "inactive-optional"),
            ("maybe-owned", "inactive-optional"),
            ("const-oid", "inactive-optional"),
            ("der", "inactive-optional"),
            ("spki", "inactive-optional"),
            ("pkcs8", "inactive-optional"),
            ("utf16string", "inactive-optional"),
            ("valuable", "inactive-optional"),
            ("document-features", "dev-dep"),
            ("litrs", "dev-dep"),
            ("itertools", "dev-dep"),
        ];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap();
        let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap();
        let mut locked: Vec<String> = Vec::new();
        let mut name = String::new();
        for line in lock.lines() {
            let line = line.trim();
            if line == "[[package]]" {
                if !name.is_empty() {
                    locked.push(name.clone());
                }
                name.clear();
            } else if let Some(v) = line.strip_prefix("name = ") {
                name = v.trim_matches('"').to_owned();
            }
        }
        if !name.is_empty() {
            locked.push(name);
        }
        assert!(!locked.is_empty(), "lock parsed");
        let groups = credit_groups();
        let mut missing = Vec::new();
        for n in &locked {
            let covered = groups
                .iter()
                .any(|g| g.crates.iter().any(|(cn, _)| cn == n));
            let skipped = NONSHIPPED.iter().any(|(sn, _)| sn == n);
            if !covered && !skipped {
                missing.push(n.clone());
            }
        }
        assert!(missing.is_empty(), "unattributed packages: {missing:?}");
    }
}
