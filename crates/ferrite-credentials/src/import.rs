//! CSV import for credentials.

use crate::credentials::CredentialRecord;

pub fn import_csv(path: &str, key: &[u8]) -> Vec<CredentialRecord> {
    if key.len() != 32 {
        return Vec::new();
    }
    let meta = std::fs::metadata(path);
    // Cap import files at 5MB to avoid OOM on adversarial CSVs.
    if meta.map(|m| m.len() > 5 * 1024 * 1024).unwrap_or(true) {
        return Vec::new();
    }
    let data = match std::fs::read_to_string(path) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for line in data.lines().skip(1) {
        if out.len() >= 10_000 {
            break;
        }
        let parts = split_csv_line(line);
        if parts.len() < 3 {
            continue;
        }
        let url = parts[0].trim().trim_matches('"');
        let username = parts[1].trim().trim_matches('"');
        let password = parts[2].trim().trim_matches('"');
        if url.is_empty() || password.is_empty() {
            continue;
        }
        // Validate URL shape before storing.
        if url::Url::parse(url).is_err() && url::Url::parse(&format!("https://{url}")).is_err() {
            continue;
        }
        out.push(CredentialRecord::new(url, username, password, key));
    }
    out
}

/// Minimal CSV split honoring double-quoted fields with `""` escapes.
fn split_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if in_quotes && chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = !in_quotes;
                }
            }
            ',' if !in_quotes => {
                fields.push(cur.clone());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    fields.push(cur);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_import() {
        let csv = "url,username,password\nhttps://example.com,user1,pass1\nhttps://a.com,,pass2\n";
        let path = std::env::temp_dir().join("ferrite_test_import.csv");
        std::fs::write(&path, csv).unwrap();
        let key = b"0123456789ABCDEF0123456789ABCDEF";
        let recs = import_csv(path.to_str().unwrap(), key);
        assert_eq!(recs.len(), 2);
        let _ = std::fs::remove_file(path);
    }
}
