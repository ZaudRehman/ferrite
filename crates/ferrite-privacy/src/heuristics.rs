//! Local phishing heuristics. No network. Suspicion only, for campaigns
//! the static list has not caught yet. The interstitial names the trigger.

/// Max distance that counts as a lookalike.
pub const LOOKALIKE_MAX_DISTANCE: usize = 2;
/// Shorter hosts are never checked (distance 2 matches nearly everything).
pub const LOOKALIKE_MIN_HOST_LEN: usize = 4;
/// Cap on the frequent host comparison set.
pub const FREQUENT_HOSTS_CAP: usize = 256;

/// Registrable domain: PSL root when known, else last two labels.
/// None for IPs, single labels, and unparseable input.
pub fn registrable(host: &str) -> Option<String> {
    let h = host.trim().to_lowercase();
    if h.is_empty() {
        return None;
    }
    if h.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    if let Ok(name) = addr::parse_domain_name(&h)
        && name.has_known_suffix()
    {
        return name.root().filter(|r| !r.is_empty()).map(str::to_owned);
    }
    let h = h.trim_matches('.');
    if h.is_empty() || h.contains(char::is_whitespace) {
        return None;
    }
    let labels: Vec<&str> = h.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|l| l.is_empty()) {
        return None;
    }
    let n = labels.len();
    Some(format!("{}.{}", labels[n - 2], labels[n - 1]))
}

/// Capped Levenshtein over chars. Returns above max early. Labels over
/// 64 chars bail at once.
pub fn edit_distance_capped(a: &str, b: &str, max: usize) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len() > 64 || b.len() > 64 {
        return max + 1;
    }
    if a.len().abs_diff(b.len()) > max {
        return max + 1;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        let mut row_min = cur[0];
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
            row_min = row_min.min(cur[j + 1]);
        }
        if row_min > max {
            return max + 1;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// First frequent domain within distance. Skips self and short hosts.
/// Comparison is registrable to registrable, never substring.
pub fn lookalike_match(host: &str, frequent: &[String]) -> Option<String> {
    let host = host.trim().to_lowercase();
    let host_reg = registrable(&host)?;
    if host_reg.len() < LOOKALIKE_MIN_HOST_LEN {
        return None;
    }
    for cand in frequent {
        if cand == &host_reg {
            continue;
        }
        if cand.len() < LOOKALIKE_MIN_HOST_LEN {
            continue;
        }
        if edit_distance_capped(&host_reg, cand, LOOKALIKE_MAX_DISTANCE) <= LOOKALIKE_MAX_DISTANCE {
            return Some(cand.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registrable_uses_psl() {
        assert_eq!(registrable("sub.bank.co.uk"), Some("bank.co.uk".to_owned()));
        assert_eq!(registrable("Example.COM"), Some("example.com".to_owned()));
        assert_eq!(registrable("127.0.0.1"), None);
        assert_eq!(registrable("::1"), None);
        assert_eq!(registrable("localhost"), None);
        assert_eq!(registrable(""), None);
        assert_eq!(registrable("sub.router.lan"), Some("router.lan".to_owned()));
        assert_eq!(registrable("shop.test"), Some("shop.test".to_owned()));
        assert_eq!(registrable("router"), None);
    }

    #[test]
    fn distance_classics_and_caps() {
        assert_eq!(edit_distance_capped("paypa1.com", "paypal.com", 2), 1);
        assert_eq!(edit_distance_capped("pay-pal.com", "paypal.com", 2), 1);
        assert_eq!(edit_distance_capped("paypal.com", "paypal.com", 2), 0);
        assert_eq!(edit_distance_capped("example.com", "unrelated.org", 2), 3);
        assert_eq!(
            edit_distance_capped("a.com", "averylonghostname.example.com", 2),
            3
        );
        assert_eq!(edit_distance_capped(&"a".repeat(70), "paypal.com", 5), 6);
    }

    #[test]
    fn lookalike_flags_near_miss_not_self_or_substring() {
        let freq = vec!["paypal.com".to_owned(), "bank.co.uk".to_owned()];
        assert_eq!(
            lookalike_match("paypa1.com", &freq),
            Some("paypal.com".to_owned())
        );
        assert_eq!(lookalike_match("paypal.com", &freq), None);
        assert_eq!(lookalike_match("secure.paypal.com.evil.com", &freq), None);
        assert_eq!(lookalike_match("kittens.example", &freq), None);
        assert_eq!(lookalike_match("a.b", &["a.c".to_owned()]), None);
    }
}
