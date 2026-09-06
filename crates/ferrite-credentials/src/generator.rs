//! Password and passphrase generation.

use ring::rand::{SecureRandom, SystemRandom};

const LOWER: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
const UPPER: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const DIGITS: &[u8] = b"0123456789";
const SYMBOLS: &[u8] = b"!@#$%^&*()-_=+[]{}|;:,.<>?";

#[derive(Debug, Clone)]
pub struct GeneratedPassword {
    pub value: String,
}

pub fn generate_password(
    len: usize,
    use_lower: bool,
    use_upper: bool,
    use_digits: bool,
    use_symbols: bool,
) -> GeneratedPassword {
    let sys = SystemRandom::new();
    let mut charset = Vec::new();
    if use_lower {
        charset.extend_from_slice(LOWER);
    }
    if use_upper {
        charset.extend_from_slice(UPPER);
    }
    if use_digits {
        charset.extend_from_slice(DIGITS);
    }
    if use_symbols {
        charset.extend_from_slice(SYMBOLS);
    }
    if charset.is_empty() {
        charset.extend_from_slice(LOWER);
        charset.extend_from_slice(DIGITS);
    }
    let mut out = String::with_capacity(len);
    let bound = 256u16 - (256u16 % charset.len() as u16);
    for _ in 0..len {
        loop {
            let mut b = [0u8; 1];
            sys.fill(&mut b).expect("CSPRNG must be available");
            if (b[0] as u16) < bound {
                let idx = (b[0] as usize) % charset.len();
                out.push(charset[idx] as char);
                break;
            }
        }
    }
    GeneratedPassword { value: out }
}

#[derive(Debug, Clone)]
pub struct PassphraseConfig {
    pub words: usize,
    pub separator: String,
}

impl Default for PassphraseConfig {
    fn default() -> Self {
        Self {
            words: 6,
            separator: "-".to_string(),
        }
    }
}

static EFF_WORDLIST_STR: &str = include_str!("../assets/eff_large_wordlist.txt");

fn eff_wordlist() -> &'static Vec<&'static str> {
    static WORDS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    WORDS.get_or_init(|| EFF_WORDLIST_STR.lines().collect())
}

pub fn generate_passphrase(cfg: &PassphraseConfig) -> GeneratedPassword {
    let sys = SystemRandom::new();
    let list = eff_wordlist();
    assert!(list.len() >= 7776, "EFF wordlist 7776 required");
    // For 7776 >256, use two-byte sampling unbiased via rejection on u16
    let mut words = Vec::new();
    for _ in 0..cfg.words {
        loop {
            let mut b = [0u8; 2];
            sys.fill(&mut b).expect("CSPRNG must be available");
            let v = u16::from_le_bytes(b) as usize;
            if v < 65536 - (65536 % list.len()) {
                let idx = v % list.len();
                words.push(list[idx]);
                break;
            }
        }
    }
    GeneratedPassword {
        value: words.join(&cfg.separator),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_length() {
        let pwd = generate_password(16, true, true, true, true);
        assert_eq!(pwd.value.len(), 16);
    }

    #[test]
    fn passphrase_words() {
        let cfg = PassphraseConfig {
            words: 4,
            separator: "-".to_string(),
        };
        let pp = generate_passphrase(&cfg);
        assert_eq!(pp.value.split('-').count(), 4);
    }
}
