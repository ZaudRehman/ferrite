//! Storage inspector.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageEntry {
    pub key: String,
    pub value: String,
    pub masked: bool,
}

#[derive(Debug, Default)]
pub struct StorageState {
    pub local: Vec<StorageEntry>,
    pub session: Vec<StorageEntry>,
    pub cookies: Vec<StorageEntry>,
    pub indexed_db: Vec<String>,
}

impl StorageState {
    pub fn clear(&mut self) {
        self.local.clear();
        self.session.clear();
        self.cookies.clear();
        self.indexed_db.clear();
    }

    pub fn mask(value: &str) -> String {
        if value.len() <= 4 {
            "••••".to_string()
        } else {
            format!("{}••••", &value[..2])
        }
    }

    pub fn query_js() -> &'static str {
        r#"(function(){
  try{
    const ls=[]; for(let i=0;i<localStorage.length;i++){ const k=localStorage.key(i); ls.push({key:k, value: localStorage.getItem(k)}); }
    const ss=[]; for(let i=0;i<sessionStorage.length;i++){ const k=sessionStorage.key(i); ss.push({key:k, value: sessionStorage.getItem(k)}); }
    const ck=document.cookie.split(';').filter(s=>s.trim()).map(s=>{ const p=s.split('='); return {key:p[0].trim(), value: p.slice(1).join('=').trim()}; });
    const idb=[]; if(window.indexedDB && indexedDB.databases) indexedDB.databases().then(dbs=>{ const names=dbs.map(d=>d.name); window.ipc.postMessage(JSON.stringify({type:'storage', local:ls, session:ss, cookies:ck, indexed_db:names})); }).catch(()=>{ window.ipc.postMessage(JSON.stringify({type:'storage', local:ls, session:ss, cookies:ck, indexed_db:[]})); }); else window.ipc.postMessage(JSON.stringify({type:'storage', local:ls, session:ss, cookies:ck, indexed_db:[]}));
  }catch(e){ window.ipc.postMessage(JSON.stringify({type:'storage', local:[], session:[], cookies:[], indexed_db:[]})) }
})();"#
    }

    pub fn edit_js(storage: &str, key: &str, value: &str) -> String {
        match storage {
            "local" => format!("localStorage.setItem({:?},{:?});", key, value),
            "session" => format!("sessionStorage.setItem({:?},{:?});", key, value),
            "cookie" => format!("document.cookie={:?}+\"=\"+{:?};", key, value),
            _ => String::new(),
        }
    }

    pub fn delete_js(storage: &str, key: &str) -> String {
        match storage {
            "local" => format!("localStorage.removeItem({:?});", key),
            "session" => format!("sessionStorage.removeItem({:?});", key),
            "cookie" => format!("document.cookie={:?}+\"=; Max-Age=0\";", key),
            _ => String::new(),
        }
    }
}
