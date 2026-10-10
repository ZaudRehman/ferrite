//! Bookmark order keys, change events, and favicon limits. Midpoint inserts
//! touch only the moved row. Rebalance when the gap crowds below MIN_GAP.

/// Wide spacing, so thousands of inserts fit before a rebalance is due.
pub const ORDER_STEP: f64 = 1024.0;
/// Below this gap the midpoint no longer separates neighbors. Rebalance,
/// then retry.
pub const MIN_GAP: f64 = 1e-6;
/// Largest favicon accepted. Bigger blobs are mislabeled downloads.
pub const MAX_FAVICON_BYTES: usize = 256 * 1024;

/// What changed. Bar, sidebar, and manager all subscribe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkEventKind {
    Inserted,
    Moved,
    Reordered,
    Renamed,
    Deleted,
    Restored,
}

/// One tree mutation. Subscribers re-read only `parent` children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkEvent {
    pub id: String,
    pub parent: Option<String>,
    pub kind: BookmarkEventKind,
}

impl BookmarkEvent {
    pub fn new(id: &str, parent: Option<String>, kind: BookmarkEventKind) -> Self {
        Self {
            id: id.to_owned(),
            parent,
            kind,
        }
    }
}

/// Midpoint between siblings. None means rebalance that set, then retry.
/// (None, None) is an empty folder.
pub fn position_between(prev: Option<f64>, next: Option<f64>) -> Option<f64> {
    match (prev, next) {
        (None, None) => Some(ORDER_STEP),
        (Some(p), None) if p.is_finite() => Some(p + ORDER_STEP),
        (None, Some(n)) if n.is_finite() => Some(n - ORDER_STEP),
        (Some(p), Some(n)) => {
            if !p.is_finite() || !n.is_finite() || n - p < MIN_GAP {
                None
            } else {
                Some(p + (n - p) / 2.0)
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_folder_starts_at_step() {
        assert_eq!(position_between(None, None), Some(ORDER_STEP));
    }

    #[test]
    fn append_and_prepend_step_out() {
        assert_eq!(position_between(Some(1024.0), None), Some(2048.0));
        assert_eq!(position_between(None, Some(1024.0)), Some(0.0));
    }

    #[test]
    fn midpoint_splits_neighbors() {
        assert_eq!(position_between(Some(1024.0), Some(2048.0)), Some(1536.0));
        assert_eq!(position_between(Some(1.0), Some(2.0)), Some(1.5));
        assert_eq!(position_between(Some(1.5), Some(2.0)), Some(1.75));
    }

    #[test]
    fn crowded_gap_requests_rebalance() {
        assert_eq!(position_between(Some(1.0), Some(1.0 + 1e-9)), None);
        assert_eq!(position_between(Some(2.0), Some(1.0)), None);
    }

    #[test]
    fn non_finite_requests_rebalance() {
        assert_eq!(position_between(Some(f64::NAN), Some(2.0)), None);
        assert_eq!(position_between(Some(1.0), Some(f64::INFINITY)), None);
    }
}
