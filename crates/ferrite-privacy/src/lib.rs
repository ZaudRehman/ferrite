//! Privacy: filtering, threat intel, and content scripts.

#![allow(clippy::collapsible_if)]
pub mod engine;
pub mod error;
pub mod filters;
pub mod headers;
pub mod js;
pub mod realtime;
pub mod threat;
pub mod updater;

pub use engine::{PrivacyEngine, PrivacySettings, VerdictCounts};
pub use error::{PrivacyError, PrivacyResult};
