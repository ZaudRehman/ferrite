//! Console bridge.

#[derive(Debug, Clone)]
pub struct ConsoleEntry {
    pub level: String,
    pub data: Vec<String>,
    pub ts: i64,
}

#[derive(Debug, Default)]
pub struct ConsoleState {
    pub entries: Vec<ConsoleEntry>,
    pub filter: String,
    pub paused: bool,
}

impl ConsoleState {
    pub fn push(&mut self, level: &str, data: Vec<String>) {
        if self.paused {
            return;
        }
        if self.entries.len() > 1000 {
            self.entries.remove(0);
        }
        self.entries.push(ConsoleEntry {
            level: level.to_owned(),
            data,
            ts: chrono::Utc::now().timestamp_millis(),
        });
        // throttle: if >1000 per second, pause
        if self.entries.len() > 1000 {
            self.paused = true;
        }
    }

    pub fn eval_js(&self, expr: &str) -> String {
        format!(
            "(function(){{ try{{ let r=eval({:?}); let s; try{{ s=JSON.stringify(r) }}catch(e){{ s=String(r) }} window.ipc.postMessage(JSON.stringify({{type:'console', level:'eval', data:[s]}})); }}catch(e){{ window.ipc.postMessage(JSON.stringify({{type:'console', level:'error', data:[String(e)]}})); }} }})();",
            expr
        )
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.paused = false;
    }

    pub fn filtered(&self) -> Vec<&ConsoleEntry> {
        if self.filter.is_empty() {
            return self.entries.iter().collect();
        }
        let needle = self.filter.to_lowercase();
        self.entries
            .iter()
            .filter(|e| {
                e.level.to_lowercase().contains(&needle)
                    || e.data.iter().any(|d| d.to_lowercase().contains(&needle))
            })
            .collect()
    }
}

pub fn console_override_js() -> &'static str {
    r#"(function(){
  const orig={};
  ['log','warn','error','info'].forEach(l=>{
    orig[l]=console[l];
    console[l]=function(...args){
      try{
        const data=args.map(a=>{
          try{ return JSON.stringify(a)}catch(e){ return String(a)}
        });
        window.ipc.postMessage(JSON.stringify({type:'console', level:l, data}));
      }catch(e){}
      return orig[l].apply(console, args);
    };
  });
})();"#
}
