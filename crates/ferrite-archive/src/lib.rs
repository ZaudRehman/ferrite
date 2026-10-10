//! Offline page archive (sanitized snapshots, FTS5, LRU) and isolated PDF rendering.

//! Nested guards stay nested: collapsing them into let-chains trades the
//! step-by-step failure narrative for no behavior gain.
#![allow(clippy::collapsible_if)]

pub mod archive;
pub mod error;
pub mod pdf;
pub mod sanitize;
pub mod shm;
pub mod storage;

pub use archive::{Archive, ArchiveConfig, Snapshot, SnapshotId};
pub use error::{ArchiveError, ArchiveResult};
