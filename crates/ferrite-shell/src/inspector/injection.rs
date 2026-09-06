//! Custom JS/CSS injection engine.

use rusqlite::{Connection, params};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct InjectionRule {
    pub id: String,
    pub pattern: String,
    pub kind: String,
    pub code: String,
}

pub fn init_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS dev_injections(id TEXT PRIMARY KEY, pattern TEXT NOT NULL, kind TEXT NOT NULL, code TEXT NOT NULL);",
    )
}

pub fn save_rule(
    conn: &Connection,
    pattern: &str,
    kind: &str,
    code: &str,
) -> rusqlite::Result<String> {
    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO dev_injections(id, pattern, kind, code) VALUES(?1,?2,?3,?4)",
        params![id, pattern, kind, code],
    )?;
    Ok(id)
}

pub fn matching_rules(conn: &Connection, url: &str) -> Vec<InjectionRule> {
    let mut stmt = match conn.prepare("SELECT id, pattern, kind, code FROM dev_injections") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = match stmt.query_map([], |r| {
        Ok(InjectionRule {
            id: r.get(0)?,
            pattern: r.get(1)?,
            kind: r.get(2)?,
            code: r.get(3)?,
        })
    }) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for r in rows.flatten() {
        if glob_match(&r.pattern, url) {
            out.push(r);
        }
    }
    out
}

fn glob_match(pattern: &str, url: &str) -> bool {
    // Very small glob: *://*.reddit.com/* -> regex
    let mut re = String::from("^");
    for c in pattern.chars() {
        match c {
            '*' => re.push_str(".*"),
            '.' => re.push_str("\\."),
            '?' => re.push('.'),
            _ => re.push(c),
        }
    }
    re.push('$');
    regex::Regex::new(&re)
        .map(|r| r.is_match(url))
        .unwrap_or(false)
}

pub fn css_injection_js(code: &str) -> String {
    format!(
        "(function(){{ const s=document.createElement('style'); s.textContent={:?}; document.documentElement.appendChild(s); }})();",
        code
    )
}

pub fn js_injection_js(code: &str) -> String {
    format!("(function(){{ {code} }})();")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn glob_matches() {
        assert!(glob_match(
            "*://*.reddit.com/*",
            "https://www.reddit.com/r/rust"
        ));
        assert!(!glob_match("*://*.reddit.com/*", "https://example.com"));
    }
    #[test]
    fn css_js_generates() {
        assert!(css_injection_js("body{color:red}").contains("createElement('style')"));
        assert!(js_injection_js("alert(1)").contains("alert"));
    }
}
