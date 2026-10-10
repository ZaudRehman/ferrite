//! Accessibility contract. The tree is engine inherited. Ferrite owns
//! composition only: child webviews under one root, one visible tab at a
//! time, focus returned from overlays. No live AT harness in CI.

use uuid::Uuid;

/// (Tab id, OS visible). Only the active tab may read visible.
pub type ExposureSnapshot = (Uuid, bool);

/// Ids visible while not active. Empty means exactly the active tab shows.
pub fn audit_exposure(active: Option<Uuid>, exposures: &[ExposureSnapshot]) -> Vec<Uuid> {
    exposures
        .iter()
        .filter(|(id, visible)| *visible && Some(*id) != active)
        .map(|(id, _)| *id)
        .collect()
}

/// Whether dismissing or activating an overlay refocuses its originating
/// tab: yes exactly when that tab is still active. A tab switch mid-menu
/// means focus already moved on.
pub fn should_return_focus(menu_tab: Uuid, active: Option<Uuid>) -> bool {
    Some(menu_tab) == active
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webview::MockHandle;

    #[test]
    fn audit_passes_for_exactly_active_visible() {
        let active = Uuid::new_v4();
        let other = Uuid::new_v4();
        let exposures = vec![(active, true), (other, false)];
        assert!(audit_exposure(Some(active), &exposures).is_empty());
    }

    #[test]
    fn audit_flags_background_tab_left_visible() {
        let active = Uuid::new_v4();
        let leaked = Uuid::new_v4();
        let exposures = vec![(active, true), (leaked, true)];
        assert_eq!(audit_exposure(Some(active), &exposures), vec![leaked]);
    }

    #[test]
    fn audit_with_no_active_tab_flags_everything_visible() {
        let a = Uuid::new_v4();
        assert_eq!(
            audit_exposure(None, &[(a, true)]),
            vec![a],
            "no active tab, nothing may expose"
        );
        assert!(audit_exposure(None, &[(a, false)]).is_empty());
    }

    #[test]
    fn audit_reads_mock_recorded_visibility() {
        let active = Uuid::new_v4();
        let background = Uuid::new_v4();
        let shown = MockHandle::new_visible(true);
        let hidden = MockHandle::new_visible(false);
        assert!(shown.was_visible());
        assert!(!hidden.was_visible());
        let exposures = vec![
            (active, shown.was_visible()),
            (background, hidden.was_visible()),
        ];
        assert!(audit_exposure(Some(active), &exposures).is_empty());
    }

    #[test]
    fn focus_returns_only_while_originating_tab_is_active() {
        let menu_tab = Uuid::new_v4();
        let other = Uuid::new_v4();
        assert!(should_return_focus(menu_tab, Some(menu_tab)));
        assert!(!should_return_focus(menu_tab, Some(other)));
        assert!(!should_return_focus(menu_tab, None));
    }
}
