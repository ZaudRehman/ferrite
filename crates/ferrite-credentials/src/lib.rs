//! Passwords, autofill, and TOTP over the encrypted vault. Plaintext exists
//! only in transit, zeroized after use.

pub mod autofill;
pub mod breach;
pub mod credentials;
pub mod generator;
pub mod health;
pub mod import;
pub mod injection;
pub mod scanner;
pub mod totp;

pub use autofill::{
    AddressFields, AutofillKind, AutofillRecord, AutofillStore, CardFields, ac_component,
};
pub use credentials::{
    CredentialRecord, CredentialStore, ImportVerdict, decrypt_field, encrypt_field,
    open_sealed_bytes, seal_bytes,
};
pub use generator::{GeneratedPassword, PassphraseConfig, generate_passphrase, generate_password};
pub use import::{CsvReport, export_csv, import_csv};
