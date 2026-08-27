//! Privacy: filtering, threat intel, and content scripts.

#![allow(clippy::collapsible_if)]
pub mod engine;
pub mod error;
pub mod filters;
pub mod headers;
pub mod js;
pub mod threat;

pub use engine::{PrivacyEngine, PrivacySettings};
pub use error::{PrivacyError, PrivacyResult};
