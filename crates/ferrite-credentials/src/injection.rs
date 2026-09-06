//! Secure injection JS.

use zeroize::Zeroize;

pub fn injection_js(selector: &str, mut value: String) -> String {
    let js = format!(
        r#"(function(){{
  const el=document.querySelector({:?});
  if(el){{
    const setter=Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype,'value').set;
    setter.call(el, {:?});
    el.dispatchEvent(new Event('input',{{bubbles:true}}));
    el.dispatchEvent(new Event('change',{{bubbles:true}}));
  }}
}})();"#,
        selector, value
    );
    value.zeroize();
    js
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn injection_contains_setter() {
        let js = injection_js("#user", "secret".to_string());
        assert!(js.contains("nativeInputValueSetter") || js.contains("HTMLInputElement"));
    }
}
