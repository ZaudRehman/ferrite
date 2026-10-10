//! Remote suggestions. Opt in only, off by default. Keystrokes leave the
//! device here. 2s timeout, fail silent. Stale replies never overwrite
//! newer lists.

/// Up to 8 phrases. Timeouts and bad payloads yield empty lists.
pub fn fetch_suggestions(query: &str) -> Vec<String> {
    use std::io::Read;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let url = format!(
        "https://duckduckgo.com/ac/?q={}",
        ferrite_tabs::protocol::encode_query_component(query)
    );
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(2)))
            .build(),
    );
    let mut body = Vec::new();
    match agent.get(&url).call() {
        Ok(resp) => {
            if resp
                .into_body()
                .into_reader()
                .take(16 * 1024)
                .read_to_end(&mut body)
                .is_err()
            {
                return Vec::new();
            }
        }
        Err(_) => return Vec::new(),
    }
    parse_suggestions(&String::from_utf8_lossy(&body))
}

/// Parse DDG payloads. Unknown shapes drop out.
pub fn parse_suggestions(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return out;
    };
    let Some(items) = v.as_array() else {
        return out;
    };
    for it in items {
        let phrase = it
            .get("phrase")
            .and_then(|p| p.as_str())
            .or_else(|| it.as_str())
            .unwrap_or("")
            .trim();
        if phrase.is_empty() || phrase.chars().count() > 100 {
            continue;
        }
        if !out.iter().any(|p| p == phrase) {
            out.push(phrase.to_owned());
        }
        if out.len() >= 8 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_phrase_objects() {
        let body = r#"[{"phrase":"rust book"},{"phrase":"rust lang"}]"#;
        assert_eq!(parse_suggestions(body), vec!["rust book", "rust lang"]);
    }

    #[test]
    fn drops_junk_shapes() {
        assert!(parse_suggestions("").is_empty());
        assert!(parse_suggestions("not json").is_empty());
        assert!(parse_suggestions(r#"{"q":"x"}"#).is_empty());
        assert!(
            parse_suggestions(r#"[{"phrase":""},{"n":1},"x",null]"#)
                .iter()
                .all(|p| !p.is_empty())
        );
        // Duplicate phrases collapse, order kept.
        let body = r#"[{"phrase":"a"},{"phrase":"a"},{"phrase":"b"}]"#;
        assert_eq!(parse_suggestions(body), vec!["a", "b"]);
    }
}
