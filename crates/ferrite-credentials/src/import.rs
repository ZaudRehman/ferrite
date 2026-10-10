//! Chrome-shaped CSV import and export. Limits: 5MB file, 10k rows.
//! Import returns the records plus an exact skip count so callers report
//! honestly instead of silently dropping malformed rows.

use crate::credentials::CredentialRecord;

/// Outcome of [`import_csv`] beyond the records themselves.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CsvReport {
    /// Data rows seen after the header.
    pub rows: usize,
    /// Rows skipped (short, bad URL, or unencryptable).
    pub skipped: usize,
}

pub fn import_csv(path: &str, key: &[u8]) -> (Vec<CredentialRecord>, CsvReport) {
    let mut report = CsvReport::default();
    if key.len() != 32 {
        return (Vec::new(), report);
    }
    let meta = std::fs::metadata(path);
    if meta.map(|m| m.len() > 5 * 1024 * 1024).unwrap_or(true) {
        return (Vec::new(), report);
    }
    let data = match std::fs::read_to_string(path) {
        Ok(d) => d,
        Err(_) => return (Vec::new(), report),
    };
    let mut out = Vec::new();
    for parts in parse_csv_records(&data).into_iter().skip(1) {
        if out.len() >= 10_000 {
            break;
        }
        report.rows += 1;
        if parts.len() < 3 {
            report.skipped += 1;
            continue;
        }
        let url = parts[0].trim();
        let username = parts[1].trim();
        let password = parts[2].trim();
        if url.is_empty() || password.is_empty() {
            report.skipped += 1;
            continue;
        }
        if url::Url::parse(url).is_err() && url::Url::parse(&format!("https://{url}")).is_err() {
            report.skipped += 1;
            continue;
        }
        match CredentialRecord::new(url, username, password, key) {
            Ok(rec) => out.push(rec),
            Err(_) => report.skipped += 1,
        }
    }
    (out, report)
}

fn parse_csv_records(data: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = data.chars().peekable();
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
                fields.push(std::mem::take(&mut cur));
            }
            '\n' if !in_quotes => {
                fields.push(std::mem::take(&mut cur));
                if fields.len() > 1 || !fields[0].trim().is_empty() {
                    records.push(std::mem::take(&mut fields));
                } else {
                    fields.clear();
                }
            }
            '\r' if !in_quotes => {
                if chars.peek() != Some(&'\n') {
                    cur.push(c);
                }
            }
            _ => cur.push(c),
        }
    }
    fields.push(cur);
    if fields.len() > 1 || !fields.join("").trim().is_empty() {
        records.push(fields);
    }
    records
}

/// Chrome-shaped CSV. Round trips through [`import_csv`]. Never sees keys.
pub fn export_csv(rows: &[(String, String, String)]) -> String {
    fn field(s: &str) -> String {
        if s.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_owned()
        }
    }
    let mut out = String::from("url,username,password\n");
    for (url, username, password) in rows {
        out.push_str(&format!(
            "{},{},{}\n",
            field(url),
            field(username),
            field(password)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_import() {
        let csv = "url,username,password\nhttps://example.com,user1,pass1\nhttps://a.com,,pass2\nbad-row\n";
        let path = std::env::temp_dir().join("ferrite_test_import.csv");
        std::fs::write(&path, csv).unwrap();
        let key = b"0123456789ABCDEF0123456789ABCDEF";
        let (recs, report) = import_csv(path.to_str().unwrap(), key);
        assert_eq!(recs.len(), 2);
        assert_eq!(report.rows, 3);
        assert_eq!(report.skipped, 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn csv_export_roundtrips() {
        let rows = vec![
            (
                "https://example.com".to_owned(),
                "user,1".to_owned(),
                "p\"ss\n1".to_owned(),
            ),
            (
                "https://a.com".to_owned(),
                String::new(),
                "pass2".to_owned(),
            ),
        ];
        let csv = export_csv(&rows);
        assert!(csv.starts_with("url,username,password\n"));
        assert!(csv.contains("\"user,1\""));
        assert!(csv.contains("\"p\"\"ss\n1\""));
        // And the importer reads back exactly what the exporter wrote.
        let path = std::env::temp_dir().join("ferrite_test_export.csv");
        std::fs::write(&path, &csv).unwrap();
        let key = b"0123456789ABCDEF0123456789ABCDEF";
        let (back, report) = import_csv(path.to_str().unwrap(), key);
        let _ = std::fs::remove_file(path);
        assert_eq!(back.len(), 2);
        assert_eq!(report.skipped, 0);
        assert_eq!(back[0].username, "user,1");
        assert_eq!(back[0].decrypt_password(key).as_deref(), Some("p\"ss\n1"));
    }
}
