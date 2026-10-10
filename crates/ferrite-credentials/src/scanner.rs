//! DOM scanner JS for autofill and TOTP.

pub fn scanner_js() -> &'static str {
    r#"(function(){
  if(window.__ferriteScannerInstalled) return;
  window.__ferriteScannerInstalled=true;
  function classify(el){
    const type=(el.getAttribute('type')||'').toLowerCase();
    const ac=(el.getAttribute('autocomplete')||'').toLowerCase();
    if(type==='password') return 'password';
    if(ac==='one-time-code') return 'totp';
    // Login identifier, pairs with a password field.
    if((type==='text'||type==='email') && /user|login|email/.test((ac+' '+el.name+' '+el.id).toLowerCase())) return 'username';
    if(['cc-number','cc-exp','cc-csc','cc-name'].includes(ac)) return 'payment';
    if(['street-address','address-line1','address-line2','postal-code','country','cc-number'].includes(ac)) return 'address';
    if(['cc-number'].includes(ac)) return 'payment';
    if(ac.includes('address')||ac.includes('postal')||ac.includes('country')) return 'address';
    return null;
  }
  function visible(el){
    const rect=el.getBoundingClientRect();
    if(rect.width===0||rect.height===0) return null;
    const style=window.getComputedStyle(el);
    if(style.display==='none'||style.visibility==='hidden'||style.opacity==='0') return null;
    return rect;
  }
  function scan(){
    const inputs=Array.from(document.querySelectorAll('input,textarea'));
    const out=[];
    for(const el of inputs){
      const rect=visible(el);
      if(!rect) continue;
      const kind=classify(el);
      if(!kind) continue;
      const ac=(el.getAttribute('autocomplete')||'').toLowerCase();
      if(kind==='payment' && ac==='cc-number'){
        const v=el.value.replace(/\D/g,'');
        if(v.length>0 && !luhn(v)) continue;
      }
      // Viewport coords. Shell matches against clientX/Y directly.
      out.push({kind, rect:{x:rect.left, y:rect.top, w:rect.width, h:rect.height}, name:el.name||'', id:el.id||'', ac, origin: location.origin});
    }
    if(out.length) try{ window.ipc.postMessage(JSON.stringify({type:'autofill_scan', top:(window===window.top), fields:out})); }catch(e){}
  }
  function luhn(s){
    let sum=0, alt=false;
    for(let i=s.length-1;i>=0;i--){ let n=parseInt(s[i],10); if(alt){ n*=2; if(n>9) n-=9; } sum+=n; alt=!alt; }
    return sum%10===0;
  }
  document.addEventListener('DOMContentLoaded', function(){ schedule(false); });
  // Trailing edge 150ms debounce. Full scan per mutation janks the page.
  let __ferriteScanTimer=null;
  function schedule(immediate){
    if(__ferriteScanTimer) return;
    const delay = immediate ? 0 : 150;
    __ferriteScanTimer=setTimeout(function(){ __ferriteScanTimer=null; scan(); }, delay);
  }
  new MutationObserver(function(){ schedule(false); }).observe(document.documentElement,{childList:true, subtree:true});
  setTimeout(function(){ schedule(false); }, 300);
  schedule(true);
  // No focusout dismiss. It would eat dropdown clicks on blur.
  let __ferriteFocusArmed=true;
  document.addEventListener('focusin',function(e){
    const el=e.target;
    if(!el||(el.tagName!=='INPUT'&&el.tagName!=='TEXTAREA')) return;
    const type=(el.getAttribute('type')||'').toLowerCase();
    if(['hidden','submit','button','checkbox','radio','file','reset','image'].includes(type)) return;
    const rect=visible(el);
    if(!rect) return;
    __ferriteFocusArmed=true;
    const ac=(el.getAttribute('autocomplete')||'').toLowerCase();
    const kind=classify(el)||'other';
    try{ window.ipc.postMessage(JSON.stringify({type:'autofill_focus',
      x:rect.left, y:rect.top, w:rect.width, h:rect.height,
      kind, ac, name:el.name||'', id:el.id||'', origin:location.origin, top:(window===window.top)})); }catch(err){}
  });
  // Dismiss-on-scroll (once per focus): no live position-update channel.
  document.addEventListener('scroll',function(){
    if(!__ferriteFocusArmed) return;
    __ferriteFocusArmed=false;
    try{ window.ipc.postMessage(JSON.stringify({type:'autofill_dismiss'})); }catch(err){}
  },true);
  // Password forms never record form history.
  document.addEventListener('submit',function(e){
    const form=e.target;
    if(!form||!form.querySelectorAll) return;
    if(form.querySelector('input[type=password]')) return;
    const entries=[];
    const inputs=Array.from(form.querySelectorAll('input,textarea'));
    for(const el of inputs){
      const type=(el.getAttribute('type')||'').toLowerCase();
      if(['hidden','password','submit','button','checkbox','radio','file','reset','image'].includes(type)) continue;
      const ac=(el.getAttribute('autocomplete')||'').toLowerCase();
      if(['cc-number','cc-exp','cc-csc','cc-name','cc-exp-month','cc-exp-year','one-time-code'].includes(ac)) continue;
      const v=(el.value||'').trim();
      if(!v) continue;
      const key=ac||el.name||el.id||'';
      if(!key) continue;
      entries.push({key, value:v.slice(0,200)});
    }
    if(!entries.length) return;
    try{ window.ipc.postMessage(JSON.stringify({type:'form_history_save', origin:location.origin, entries})); }catch(err){}
  },true);
  // Report only. Shell saves on explicit user action. Once per user per doc.
  document.addEventListener('submit',function(e){
    const form=e.target;
    if(!form||!form.querySelectorAll) return;
    const pw=form.querySelector('input[type=password]');
    if(!pw||!pw.value) return;
    let user='';
    const ids=Array.from(form.querySelectorAll('input[type=text],input[type=email],input:not([type])'));
    for(const el of ids){ const v=(el.value||'').trim(); if(v){ user=v; break; } }
    if(!user) return;
    window.__ferriteSaveSent=window.__ferriteSaveSent||{};
    const key=user+'@'+location.origin;
    if(window.__ferriteSaveSent[key]) return;
    window.__ferriteSaveSent[key]=true;
    try{ window.ipc.postMessage(JSON.stringify({type:'password_save_candidate', origin:location.origin, username:user.slice(0,256), password:pw.value.slice(0,4096)})); }catch(err){}
  },true);
  // Type ahead while the shell holds the dropdown open.
  let __ferriteAfFilterTimer=null;
  document.addEventListener('keydown',function(e){
    if(!window.__ferriteAfOpen) return;
    const t=e.target;
    if(!t||(t.tagName!=='INPUT'&&t.tagName!=='TEXTAREA')) return;
    if(e.key==='ArrowDown'||e.key==='ArrowUp'){
      e.preventDefault();
      try{ window.ipc.postMessage(JSON.stringify({type:'af_nav',dir:e.key==='ArrowDown'?1:-1})); }catch(err){}
    }
    else if(e.key==='Enter'){
      e.preventDefault();
      try{ window.ipc.postMessage(JSON.stringify({type:'af_pick'})); }catch(err){}
    }
    else if(e.key==='Escape'){
      try{ window.ipc.postMessage(JSON.stringify({type:'autofill_dismiss'})); }catch(err){}
    }
  },true);
  document.addEventListener('input',function(e){
    if(!window.__ferriteAfOpen) return;
    const t=e.target;
    if(!t||(t.tagName!=='INPUT'&&t.tagName!=='TEXTAREA')) return;
    const val=t.value||'';
    if(__ferriteAfFilterTimer) clearTimeout(__ferriteAfFilterTimer);
    __ferriteAfFilterTimer=setTimeout(function(){
      try{ window.ipc.postMessage(JSON.stringify({type:'af_filter',value:val.slice(0,100)})); }catch(err){}
    },120);
  });
  // Blob URLs stay in page. Page streams slices back on shell invoke.
  window.__ferriteSaveBlob=function(url,id){
    fetch(url).then(function(r){
      if(!r.ok) throw new Error('status '+r.status);
      var mime=(r.headers.get('content-type')||'').split(';')[0].trim();
      return r.arrayBuffer().then(function(buf){ return {buf:buf,mime:mime}; });
    }).then(function(o){
      var bytes=new Uint8Array(o.buf);
      var CH=36864;
      for(var off=0;off<bytes.length;off+=CH){
        var slice=bytes.subarray(off,Math.min(off+CH,bytes.length));
        var bin='';
        for(var i=0;i<slice.length;i++) bin+=String.fromCharCode(slice[i]);
        try{ window.ipc.postMessage(JSON.stringify({type:'blob_chunk',id:id,seq:off/CH,data:btoa(bin)})); }catch(err){ return; }
      }
      try{ window.ipc.postMessage(JSON.stringify({type:'blob_end',id:id,mime:o.mime})); }catch(err){}
    }).catch(function(){
      try{ window.ipc.postMessage(JSON.stringify({type:'blob_end',id:id,mime:''})); }catch(err){}
    });
  };
})();"#
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scanner_contains_luhn() {
        assert!(scanner_js().contains("luhn"));
        assert!(scanner_js().contains("one-time-code"));
    }

    #[test]
    fn scanner_posts_focus_and_submit() {
        let js = scanner_js();
        assert!(js.contains("autofill_focus"), "focus trigger");
        assert!(js.contains("autofill_dismiss"), "scroll dismiss");
        assert!(js.contains("form_history_save"), "submit capture");
        assert!(js.contains("input[type=password]"), "password-form guard");
    }

    #[test]
    fn scanner_marks_top_frame_on_reports() {
        let js = scanner_js();
        assert!(js.contains("top:(window===window.top)"), "frame flag");
    }

    #[test]
    fn scanner_posts_password_save_candidate() {
        let js = scanner_js();
        assert!(js.contains("password_save_candidate"), "candidate post");
        assert!(js.contains("if(!user) return"), "username gate");
        assert!(js.contains("if(!pw||!pw.value) return"), "empty gate");
        assert!(js.contains("__ferriteSaveSent"), "dedupe gate");
        assert!(js.contains("slice(0,256)"), "username cap");
        assert!(js.contains("slice(0,4096)"), "password cap");
        let pos = js.find("password_save_candidate").unwrap_or(0);
        let start = js[..pos]
            .rfind("document.addEventListener('submit'")
            .unwrap_or(0);
        let end = js[pos..]
            .find("},true);")
            .map(|i| pos + i)
            .unwrap_or(js.len());
        assert!(
            !js[start..end].contains("preventDefault"),
            "no submit block"
        );
    }

    #[test]
    fn scanner_typeahead_channel() {
        let js = scanner_js();
        assert!(js.contains("__ferriteAfOpen"), "open flag");
        assert!(js.contains("af_nav"), "arrow forwarding");
        assert!(js.contains("af_pick"), "enter forwarding");
        assert!(js.contains("af_filter"), "value filtering");
    }

    #[test]
    fn scanner_blob_rescue() {
        let js = scanner_js();
        assert!(js.contains("__ferriteSaveBlob"), "blob entry");
        assert!(js.contains("blob_chunk"), "chunk posts");
        assert!(js.contains("blob_end"), "completion post");
    }
}
