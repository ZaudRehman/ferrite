//! Elements inspector: DOM tree and styles.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomNode {
    pub tag: String,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<DomNode>,
    pub styles: Vec<(String, String)>,
    pub computed: Vec<(String, String)>,
    pub box_model: Option<BoxModel>,
    pub listeners: Vec<String>,
    pub a11y: Option<A11yNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoxModel {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub margin: [f32; 4],
    pub padding: [f32; 4],
    pub border: [f32; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A11yNode {
    pub role: String,
    pub name: Option<String>,
    pub level: Option<u32>,
}

#[derive(Debug, Default)]
pub struct ElementsState {
    pub root: Option<DomNode>,
    pub selected: Option<String>,
    pub style_edits: Vec<(String, String, String)>,
}

impl ElementsState {
    pub fn clear(&mut self) {
        self.root = None;
        self.selected = None;
        self.style_edits.clear();
    }

    pub fn set_tree(&mut self, node: DomNode) {
        self.root = Some(node);
    }

    pub fn edit_style(&mut self, selector: &str, prop: &str, value: &str) -> String {
        self.style_edits
            .push((selector.to_owned(), prop.to_owned(), value.to_owned()));
        format!(
            "(function(){{ const el=document.querySelector({:?}); if(el) el.style.setProperty({:?},{:?}); }})();",
            selector, prop, value
        )
    }

    pub fn highlight_js(&self, selector: &str) -> String {
        format!(
            "(function(){{ let e=document.querySelector({:?}); if(!e) return; let o=document.getElementById('__ferrite_hl'); if(!o){{o=document.createElement('div'); o.id='__ferrite_hl'; o.style.cssText='position:fixed;pointer-events:none;z-index:2147483647;border:2px solid #4F46E5;background:rgba(79,70,229,0.15)'; document.body.appendChild(o);}} let r=e.getBoundingClientRect(); o.style.left=r.left+'px'; o.style.top=r.top+'px'; o.style.width=r.width+'px'; o.style.height=r.height+'px'; }})();",
            selector
        )
    }
}

pub fn dom_scanner_js() -> &'static str {
    r#"(function(){
  function ser(el){
    const s=window.getComputedStyle(el);
    const keys=['color','background-color','display','font-size','font-family','margin','padding','border','position','width','height','opacity','z-index'];
    const computed=keys.map(k=>[k, s.getPropertyValue(k)]);
    const styles=[['color',s.color],['background-color',s.backgroundColor],['display',s.display],['font-size',s.fontSize]];
    const r=el.getBoundingClientRect();
    const box_model={x:r.x,y:r.y,width:r.width,height:r.height,margin:[parseFloat(s.marginTop)||0,parseFloat(s.marginRight)||0,parseFloat(s.marginBottom)||0,parseFloat(s.marginLeft)||0],padding:[parseFloat(s.paddingTop)||0,parseFloat(s.paddingRight)||0,parseFloat(s.paddingBottom)||0,parseFloat(s.paddingLeft)||0],border:[parseFloat(s.borderTopWidth)||0,parseFloat(s.borderRightWidth)||0,parseFloat(s.borderBottomWidth)||0,parseFloat(s.borderLeftWidth)||0]};
    const attrs=[]; for(const a of el.attributes) attrs.push([a.name,a.value]);
    const listeners=(getEventListeners?Object.keys(getEventListeners(el)):[]);
    const a11y={role: el.getAttribute('role')||el.tagName.toLowerCase(), name: el.getAttribute('aria-label')||el.innerText?.slice(0,120)||null, level: el.getAttribute('aria-level')?parseInt(el.getAttribute('aria-level')):null};
    const children=[]; for(const c of el.children) children.push(ser(c));
    return {tag:el.tagName.toLowerCase(), id:el.id||null, classes: Array.from(el.classList), attrs, children, styles, computed, box_model, listeners, a11y};
  }
  const root=ser(document.documentElement);
  try{ window.ipc.postMessage(JSON.stringify({type:'elements', tree: root})); }catch(e){}
  new MutationObserver(()=>{ setTimeout(()=>{ try{ const r=ser(document.documentElement); window.ipc.postMessage(JSON.stringify({type:'elements', tree: r})); }catch(e){} },100); }).observe(document.documentElement,{childList:true, subtree:true, attributes:true});
})();"#
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edit_generates_js() {
        let mut s = ElementsState::default();
        let js = s.edit_style("#a", "color", "red");
        assert!(js.contains("setProperty"));
        assert!(js.contains("red"));
    }
}
