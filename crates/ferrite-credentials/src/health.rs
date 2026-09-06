//! Password health dashboard - via vault and breached hyper checks.

use crate::credentials::CredentialRecord;

pub fn is_weak(password: &str) -> bool {
    if password.len() < 12 {
        return true;
    }
    let has_lower = password.chars().any(|c| c.is_ascii_lowercase());
    let has_upper = password.chars().any(|c| c.is_ascii_uppercase());
    let has_digit = password.chars().any(|c| c.is_ascii_digit());
    let has_symbol = password.chars().any(|c| !c.is_ascii_alphanumeric());
    let diversity = [has_lower, has_upper, has_digit, has_symbol]
        .iter()
        .filter(|&&b| b)
        .count();
    diversity < 3
}

pub fn find_reused(creds: &[CredentialRecord], key: &[u8]) -> Vec<String> {
    use std::collections::HashMap;
    use zeroize::Zeroize;
    let mut map: HashMap<Vec<u8>, Vec<String>> = HashMap::new();
    for c in creds {
        // Skip undecryptable rows: never bucket garbage together as "reused".
        if let Some(pwd) = c.decrypt_password(key) {
            map.entry(pwd.into_bytes()).or_default().push(c.id.clone());
        }
    }
    let mut out = Vec::new();
    for (mut pwd, ids) in map {
        if ids.len() > 1 {
            out.extend(ids);
        }
        pwd.zeroize();
    }
    out
}

pub fn is_old(hlc: i64, threshold_days: i64) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    now - hlc > threshold_days * 24 * 60 * 60 * 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weak_detection() {
        assert!(is_weak("123"));
        assert!(!is_weak("aB3!aB3!aB3!aB3!"));
    }

    #[test]
    fn reused_detection() {
        let key = b"0123456789ABCDEF0123456789ABCDEF";
        let c1 =
            crate::credentials::CredentialRecord::new("https://a.com", "u", "same", key).unwrap();
        let c2 =
            crate::credentials::CredentialRecord::new("https://b.com", "u", "same", key).unwrap();
        let c3 =
            crate::credentials::CredentialRecord::new("https://c.com", "u", "diff", key).unwrap();
        let reused = find_reused(&[c1, c2, c3], key);
        assert_eq!(reused.len(), 2);
    }
}
