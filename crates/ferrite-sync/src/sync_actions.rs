//! Sync actions across every promised category.

use serde::{Deserialize, Serialize};

/// Closed set. Trust exceptions and extensions have no variant: exceptions
/// are device specific, extension folders are local only. History needs
/// explicit opt in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Copy)]
pub enum SyncCategory {
    Bookmarks,
    History,
    Credentials,
    Autofill,
    Totp,
    Archive,
    TrustedDevices,
}

impl SyncCategory {
    pub fn all() -> &'static [SyncCategory] {
        &[
            SyncCategory::Bookmarks,
            SyncCategory::History,
            SyncCategory::Credentials,
            SyncCategory::Autofill,
            SyncCategory::Totp,
            SyncCategory::Archive,
            SyncCategory::TrustedDevices,
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncAction {
    AddBookmark {
        id: String,
        url: String,
        title: String,
        parent_folder_id: Option<String>,
        /// Fractional sibling position. Defaults to 0.0 for old snapshots.
        #[serde(default)]
        sort_order: f64,
        hlc_timestamp: u64,
    },
    DeleteBookmark {
        id: String,
        hlc_timestamp: u64,
    },
    UpsertHistoryEntry {
        id: String,
        url: String,
        title: String,
        visit_count: u32,
        last_visit_timestamp: i64,
        hlc_timestamp: u64,
    },
    AddWebSnapshotMetadata {
        id: String,
        url: String,
        capture_timestamp: i64,
        is_pinned: bool,
        hlc_timestamp: u64,
    },
    UpdateCredential {
        id: String,
        encrypted_password: Vec<u8>,
        hlc_timestamp: u64,
    },
    DeleteCredential {
        id: String,
        hlc_timestamp: u64,
    },
    UpdateAutofillRecord {
        id: String,
        encrypted_fields: Vec<u8>,
        hlc_timestamp: u64,
    },
    DeleteAutofillRecord {
        id: String,
        hlc_timestamp: u64,
    },
    UpsertTotpSecret {
        credential_id: String,
        encrypted_secret: Vec<u8>,
        digits: u8,
        period_seconds: u32,
        hlc_timestamp: u64,
    },
    AddTrustedDevice {
        device_id: String,
        device_name: String,
        public_key: Vec<u8>,
        paired_via_device_id: Option<String>,
        hlc_timestamp: u64,
    },
    RevokeTrustedDevice {
        device_id: String,
        hlc_timestamp: u64,
    },
}

impl SyncAction {
    pub fn category(&self) -> SyncCategory {
        match self {
            SyncAction::AddBookmark { .. } | SyncAction::DeleteBookmark { .. } => {
                SyncCategory::Bookmarks
            }
            SyncAction::UpsertHistoryEntry { .. } => SyncCategory::History,
            SyncAction::AddWebSnapshotMetadata { .. } => SyncCategory::Archive,
            SyncAction::UpdateCredential { .. } | SyncAction::DeleteCredential { .. } => {
                SyncCategory::Credentials
            }
            SyncAction::UpdateAutofillRecord { .. } | SyncAction::DeleteAutofillRecord { .. } => {
                SyncCategory::Autofill
            }
            SyncAction::UpsertTotpSecret { .. } => SyncCategory::Totp,
            SyncAction::AddTrustedDevice { .. } | SyncAction::RevokeTrustedDevice { .. } => {
                SyncCategory::TrustedDevices
            }
        }
    }

    pub fn hlc(&self) -> u64 {
        match self {
            SyncAction::AddBookmark { hlc_timestamp, .. }
            | SyncAction::DeleteBookmark { hlc_timestamp, .. }
            | SyncAction::UpsertHistoryEntry { hlc_timestamp, .. }
            | SyncAction::AddWebSnapshotMetadata { hlc_timestamp, .. }
            | SyncAction::UpdateCredential { hlc_timestamp, .. }
            | SyncAction::DeleteCredential { hlc_timestamp, .. }
            | SyncAction::UpdateAutofillRecord { hlc_timestamp, .. }
            | SyncAction::DeleteAutofillRecord { hlc_timestamp, .. }
            | SyncAction::UpsertTotpSecret { hlc_timestamp, .. }
            | SyncAction::AddTrustedDevice { hlc_timestamp, .. }
            | SyncAction::RevokeTrustedDevice { hlc_timestamp, .. } => *hlc_timestamp,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CrdtMessage {
    SyncRequest {
        since_vector_clock: std::collections::HashMap<String, u64>,
        enabled_categories: std::collections::HashSet<SyncCategory>,
    },
    SyncResponse {
        deltas: Vec<SyncAction>,
    },
    FetchArchiveBody {
        snapshot_id: String,
    },
    FetchArchiveBodyResponse {
        snapshot_id: String,
        compressed_html: Vec<u8>,
        /// HLC of the snapshot row at the sender. Old peers omit it
        /// (default 0): receivers fill empty bodies but never overwrite
        /// a filled body with an unguarded (0) payload.
        #[serde(default)]
        hlc_timestamp: u64,
    },
    /// Best-effort live-tab presence push (ephemeral, never persisted).
    /// Replaces the deleted Raft-based PushTab: tab presence needs no
    /// consensus, only delivery + expiry (see `CrdtEngine::live_tabs`).
    PushLiveTab {
        action: LiveTabAction,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LiveTabAction {
    PushTab {
        url: String,
        source_device: String,
        target_device: String,
        timestamp: i64,
    },
}
