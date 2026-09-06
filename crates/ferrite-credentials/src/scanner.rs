//! DOM scanner JS for autofill and TOTP.

pub fn scanner_js() -> &'static str {
    r#"(function(){
  if(window.__ferriteScannerInstalled) return;
  window.__ferriteScannerInstalled=true;
  function scan(){
    const inputs=Array.from(document.querySelectorAll('input'));
    const out=[];
    for(const el of inputs){
      const type=(el.getAttribute('type')||'').toLowerCase();
      const ac=(el.getAttribute('autocomplete')||'').toLowerCase();
      const rect=el.getBoundingClientRect();
      if(rect.width===0||rect.height===0) continue;
      const style=window.getComputedStyle(el);
      if(style.display==='none'||style.visibility==='hidden'||style.opacity==='0') continue;
      let kind=null;
      if(type==='password') kind='password';
      else if(ac==='one-time-code') kind='totp';
      else if(['cc-number','cc-exp','cc-csc','cc-name'].includes(ac)) kind='payment';
      else if(['street-address','address-line1','address-line2','postal-code','country','cc-number'].includes(ac)) kind='address';
      else if(['cc-number'].includes(ac)) kind='payment';
      else if(ac.includes('address')||ac.includes('postal')||ac.includes('country')) kind='address';
      if(!kind) continue;
      // Luhn for cc-number
      if(kind==='payment' && ac==='cc-number'){
        const v=el.value.replace(/\D/g,'');
        if(v.length>0 && !luhn(v)) continue;
      }
      out.push({kind, rect:{x:rect.left+window.scrollX, y:rect.top+window.scrollY, w:rect.width, h:rect.height}, name:el.name||'', id:el.id||'', ac, origin: location.origin});
    }
    if(out.length) try{ window.ipc.postMessage(JSON.stringify({type:'autofill_scan', fields:out})); }catch(e){}
  }
  function luhn(s){
    let sum=0, alt=false;
    for(let i=s.length-1;i>=0;i--){ let n=parseInt(s[i],10); if(alt){ n*=2; if(n>9) n-=9; } sum+=n; alt=!alt; }
    return sum%10===0;
  }
  document.addEventListener('DOMContentLoaded', function(){ schedule(false); });
  // Debounced observer: SPA/React pages fire mutations per keystroke; an
  // undebounced full querySelectorAll+getComputedStyle scan per mutation
  // janks the page. Trailing-edge 150ms debounce coalesces bursts.
  let __ferriteScanTimer=null;
  function schedule(immediate){
    if(__ferriteScanTimer) return;
    const delay = immediate ? 0 : 150;
    __ferriteScanTimer=setTimeout(function(){ __ferriteScanTimer=null; scan(); }, delay);
  }
  new MutationObserver(function(){ schedule(false); }).observe(document.documentElement,{childList:true, subtree:true});
  setTimeout(function(){ schedule(false); }, 300);
  schedule(true);
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
}
