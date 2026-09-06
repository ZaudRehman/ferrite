//! Sovereign Archive and native PDF engine.

#![allow(clippy::collapsible_if)]

pub mod archive;
pub mod error;
pub mod pdf;
pub mod sanitize;
pub mod shm;
pub mod storage;

pub use archive::{Archive, ArchiveConfig, Snapshot, SnapshotId};
pub use error::{ArchiveError, ArchiveResult};
