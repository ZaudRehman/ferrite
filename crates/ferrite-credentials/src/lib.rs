//! Credentials and autofill manager.

#![allow(clippy::collapsible_if, dead_code)]

pub mod breach;
pub mod credentials;
pub mod generator;
pub mod health;
pub mod import;
pub mod injection;
pub mod scanner;
pub mod sync;
pub mod totp;

pub use credentials::{CredentialRecord, CredentialStore};
pub use generator::{GeneratedPassword, PassphraseConfig, generate_passphrase, generate_password};
