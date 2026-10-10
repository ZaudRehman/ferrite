//! Address and card records. Cards never store a CVV. Deletes tombstone
//! (`is_deleted=1`) so sync propagates them like credential deletes.

use std::sync::Arc;

use rusqlite::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroize;

use ferrite_vault::Vault;

use crate::credentials::{decrypt_field, encrypt_field};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AddressFields {
    pub label: String,
    pub name: String,
    pub street: String,
    pub city: String,
    pub region: String,
    pub postal: String,
    pub country: String,
    pub phone: String,
    pub email: String,
}

/// No CVV member by design.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CardFields {
    pub label: String,
    pub name: String,
    pub pan: String,
    pub expiry: String,
    pub network: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutofillKind {
    Address,
    Card,
}

impl AutofillKind {
    fn as_str(self) -> &'static str {
        match self {
            AutofillKind::Address => "address",
            AutofillKind::Card => "card",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "address" => Some(AutofillKind::Address),
            "card" => Some(AutofillKind::Card),
            _ => None,
        }
    }
}

pub struct AutofillRecord {
    pub id: String,
    pub kind: AutofillKind,
    pub label: String,
    pub fields_json: String,
    pub added: i64,
}

pub struct AutofillStore {
    vault: Arc<Vault>,
}

impl AutofillStore {
    pub fn new(vault: Arc<Vault>) -> Self {
        Self { vault }
    }

    pub fn save_address(&self, fields: &AddressFields, key: &[u8]) -> Result<String, String> {
        validate_address(fields)?;
        let json = serde_json::to_string(fields).map_err(|e| format!("encode failed: {e}"))?;
        self.insert(AutofillKind::Address, &fields.label, &json, key)
    }

    pub fn save_card(&self, fields: &mut CardFields, key: &[u8]) -> Result<String, String> {
        validate_card(fields)?;
        let digits: String = fields.pan.chars().filter(|c| c.is_ascii_digit()).collect();
        fields.network = card_network(&digits).to_owned();
        fields.pan = digits;
        let json = serde_json::to_string(&*fields).map_err(|e| format!("encode failed: {e}"))?;
        fields.pan.zeroize();
        self.insert(AutofillKind::Card, &fields.label, &json, key)
    }

    fn insert(
        &self,
        kind: AutofillKind,
        label: &str,
        json: &str,
        key: &[u8],
    ) -> Result<String, String> {
        let enc = encrypt_field(json.as_bytes(), key)?;
        let id = Uuid::new_v4().to_string();
        let conn = self
            .vault
            .conn
            .lock()
            .map_err(|_| "vault lock poisoned".to_owned())?;
        conn.execute(
            "INSERT INTO autofill_records(id, record_type, label, encrypted_fields, hlc_timestamp) VALUES(?1,?2,?3,?4,?5)",
            params![id, kind.as_str(), label, enc, now_millis()],
        )
        .map_err(|e| format!("store failed: {e}"))?;
        Ok(id)
    }

    pub fn list(&self, kind: AutofillKind, key: &[u8]) -> Vec<AutofillRecord> {
        let conn = match self.vault.conn.lock() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let mut stmt = match conn.prepare(
            "SELECT id, label, encrypted_fields, hlc_timestamp FROM autofill_records WHERE record_type=?1 AND is_deleted=0 ORDER BY hlc_timestamp DESC LIMIT 500",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([kind.as_str()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, i64>(3)?,
            ))
        });
        let mut out = Vec::new();
        if let Ok(rows) = rows {
            for r in rows.flatten() {
                let Ok(plain) = decrypt_field(&r.2, key) else {
                    continue;
                };
                let Ok(json) = String::from_utf8(plain) else {
                    continue;
                };
                out.push(AutofillRecord {
                    id: r.0,
                    kind,
                    label: r.1,
                    fields_json: json,
                    added: r.3,
                });
            }
        }
        out
    }

    pub fn get(&self, id: &str, key: &[u8]) -> Option<AutofillRecord> {
        let conn = self.vault.conn.lock().ok()?;
        let (kind, label, enc, added): (String, String, Vec<u8>, i64) = conn
            .query_row(
                "SELECT record_type, label, encrypted_fields, hlc_timestamp FROM autofill_records WHERE id=?1 AND is_deleted=0",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .ok()?;
        let plain = decrypt_field(&enc, key).ok()?;
        let json = String::from_utf8(plain).ok()?;
        Some(AutofillRecord {
            id: id.to_owned(),
            kind: AutofillKind::parse(&kind)?,
            label,
            fields_json: json,
            added,
        })
    }

    /// Tombstone, never hard delete, so the sync delta emitter picks
    /// the delete up via `is_deleted` like credential deletes.
    pub fn delete(&self, id: &str) -> Result<(), String> {
        let conn = self
            .vault
            .conn
            .lock()
            .map_err(|_| "vault lock poisoned".to_owned())?;
        conn.execute(
            "UPDATE autofill_records SET is_deleted=1, hlc_timestamp=?1 WHERE id=?2",
            params![now_millis(), id],
        )
        .map_err(|e| format!("delete failed: {e}"))?;
        Ok(())
    }
}

fn validate_address(f: &AddressFields) -> Result<(), String> {
    if f.label.trim().is_empty() {
        return Err("label required".to_owned());
    }
    if f.name.trim().is_empty()
        && f.street.trim().is_empty()
        && f.postal.trim().is_empty()
        && f.email.trim().is_empty()
        && f.phone.trim().is_empty()
    {
        return Err("at least one field required".to_owned());
    }
    Ok(())
}

fn validate_card(f: &CardFields) -> Result<(), String> {
    if f.label.trim().is_empty() {
        return Err("label required".to_owned());
    }
    let digits: String = f.pan.chars().filter(|c| c.is_ascii_digit()).collect();
    if !(13..=19).contains(&digits.len()) || !luhn(&digits) {
        return Err("card number failed checksum".to_owned());
    }
    let exp = f.expiry.trim();
    if exp.len() < 4
        || exp.len() > 7
        || !exp.contains('/')
        || !exp.chars().all(|c| c.is_ascii_digit() || c == '/')
    {
        return Err("expiry looks like MM/YY".to_owned());
    }
    Ok(())
}

pub fn luhn(digits: &str) -> bool {
    let mut sum = 0u32;
    let mut alt = false;
    for c in digits.chars().rev() {
        let Some(mut n) = c.to_digit(10) else {
            return false;
        };
        if alt {
            n *= 2;
            if n > 9 {
                n -= 9;
            }
        }
        sum += n;
        alt = !alt;
    }
    !digits.is_empty() && sum.is_multiple_of(10)
}

/// Static BIN table. No network lookup.
pub fn card_network(pan_digits: &str) -> &'static str {
    if pan_digits.starts_with('4') {
        return "Visa";
    }
    if pan_digits.len() >= 2
        && let Ok(n) = pan_digits[..2].parse::<u32>()
    {
        if (51..=55).contains(&n) {
            return "Mastercard";
        }
        if n == 34 || n == 37 {
            return "Amex";
        }
    }
    if pan_digits.len() >= 4
        && let Ok(n) = pan_digits[..4].parse::<u32>()
    {
        if (2221..=2720).contains(&n) {
            return "Mastercard";
        }
        if n == 6011 || (644..=649).contains(&n) {
            return "Discover";
        }
    }
    if pan_digits.starts_with("65") {
        return "Discover";
    }
    "Unknown"
}

pub fn mask_pan(pan_digits: &str) -> String {
    match pan_digits.len() {
        0..=4 => "••••".to_owned(),
        n => format!("•••• •••• •••• {}", &pan_digits[n - 4..]),
    }
}

/// `cc-csc` maps to None. CVV is never filled.
pub fn ac_component(ac: &str) -> Option<(AutofillKind, &'static str)> {
    let ac = ac.to_lowercase();
    match ac.as_str() {
        "cc-number" => Some((AutofillKind::Card, "pan")),
        "cc-exp" => Some((AutofillKind::Card, "expiry")),
        "cc-name" => Some((AutofillKind::Card, "name")),
        "cc-csc" => None,
        "street-address" | "address-line1" | "address-line2" => {
            Some((AutofillKind::Address, "street"))
        }
        "address-level1" => Some((AutofillKind::Address, "region")),
        "address-level2" => Some((AutofillKind::Address, "city")),
        "postal-code" => Some((AutofillKind::Address, "postal")),
        "country" => Some((AutofillKind::Address, "country")),
        "name" => Some((AutofillKind::Address, "name")),
        "email" => Some((AutofillKind::Address, "email")),
        "tel" => Some((AutofillKind::Address, "phone")),
        _ => {
            if ac.contains("postal") {
                Some((AutofillKind::Address, "postal"))
            } else if ac.contains("country") {
                Some((AutofillKind::Address, "country"))
            } else if ac.contains("address") {
                Some((AutofillKind::Address, "street"))
            } else {
                None
            }
        }
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luhn_vectors() {
        assert!(luhn("4111111111111111"));
        assert!(luhn("5555555555554444"));
        assert!(luhn("378282246310005"));
        assert!(!luhn("4111111111111112"));
        assert!(!luhn(""));
        assert!(!luhn("4111a11111111111"));
    }

    #[test]
    fn network_table() {
        assert_eq!(card_network("4111111111111111"), "Visa");
        assert_eq!(card_network("5555555555554444"), "Mastercard");
        assert_eq!(card_network("2221000000000000"), "Mastercard");
        assert_eq!(card_network("378282246310005"), "Amex");
        assert_eq!(card_network("6011111111111117"), "Discover");
        assert_eq!(card_network("6500000000000000"), "Discover");
        assert_eq!(card_network("9000000000000000"), "Unknown");
    }

    #[test]
    fn mask_shows_last4_only() {
        assert_eq!(mask_pan("4111111111111111"), "•••• •••• •••• 1111");
        assert_eq!(mask_pan("12"), "••••");
    }

    #[test]
    fn csc_never_maps() {
        assert_eq!(ac_component("cc-csc"), None);
        assert_eq!(ac_component("cc-number"), Some((AutofillKind::Card, "pan")));
        assert_eq!(
            ac_component("postal-code"),
            Some((AutofillKind::Address, "postal"))
        );
    }

    #[test]
    fn card_validation_rejects() {
        let mut good = CardFields {
            label: "Personal".to_owned(),
            name: "A U Thor".to_owned(),
            pan: "4111 1111 1111 1111".to_owned(),
            expiry: "12/30".to_owned(),
            network: String::new(),
        };
        assert!(validate_card(&good).is_ok());
        good.pan = "4111111111111112".to_owned();
        assert!(validate_card(&good).is_err());
        good.pan = "4111111111111111".to_owned();
        good.label = "  ".to_owned();
        assert!(validate_card(&good).is_err());
    }

    #[test]
    fn store_round_trip() {
        let vault = Arc::new(Vault::open_in_memory().unwrap());
        let store = AutofillStore::new(vault);
        let key = b"0123456789ABCDEF0123456789ABCDEF";
        let addr = AddressFields {
            label: "Home".to_owned(),
            name: "A U Thor".to_owned(),
            postal: "12345".to_owned(),
            ..Default::default()
        };
        let id = store.save_address(&addr, key).unwrap();
        let rows = store.list(AutofillKind::Address, key);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        let back: AddressFields = serde_json::from_str(&rows[0].fields_json).unwrap();
        assert_eq!(back.postal, "12345");
        store.delete(&id).unwrap();
        assert!(store.list(AutofillKind::Address, key).is_empty());
    }

    #[test]
    fn delete_tombstones_for_sync_while_hiding_from_reads() {
        let vault = Arc::new(Vault::open_in_memory().unwrap());
        {
            let conn = vault.conn.lock().unwrap();
            crate::credentials::CredentialStore::init_schema(&conn).unwrap();
        }
        let store = AutofillStore::new(vault.clone());
        let key = b"0123456789ABCDEF0123456789ABCDEF";
        let addr = AddressFields {
            label: "Home".to_owned(),
            name: "A U Thor".to_owned(),
            postal: "12345".to_owned(),
            ..Default::default()
        };
        let id = store.save_address(&addr, key).unwrap();
        assert!(store.get(&id, key).is_some());
        store.delete(&id).unwrap();
        // Hidden from reads ...
        assert!(store.list(AutofillKind::Address, key).is_empty());
        assert!(store.get(&id, key).is_none());
        // ... but the tombstone row survives for the sync emitter.
        let conn = vault.conn.lock().unwrap();
        let (deleted, hlc): (i64, i64) = conn
            .query_row(
                "SELECT is_deleted, hlc_timestamp FROM autofill_records WHERE id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(deleted, 1);
        assert!(hlc > 0);
    }
}
