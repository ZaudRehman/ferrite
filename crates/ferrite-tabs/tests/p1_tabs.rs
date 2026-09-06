//! P1-17 gates: traversal preserves forward history, pinned close errors,
//! close never fabricates webviews, session round-trips, reload/stop semantics.

use std::rc::Rc;

use ferrite_tabs::{MockFactory, TabError, TabManager};

fn manager() -> TabManager {
    TabManager::new(Rc::new(MockFactory))
}

#[test]
fn back_then_forward_preserved_via_traversal() {
    let mut m = manager();
    m.new_tab_mock(Some("https://a.com".to_owned())).unwrap();
    let tab = m.active_tab_mut().unwrap();
    tab.load_url("https://b.com".to_owned()).unwrap();
    tab.load_url("https://c.com".to_owned()).unwrap();
    assert_eq!(m.active_tab().unwrap().url, "https://c.com");

    // Back: traversal must NOT destroy the forward stack (old bug:
    // go_back + load_url re-pushed and cleared forward).
    let back = m.active_tab_mut().unwrap().go_back().unwrap();
    assert_eq!(back, "https://b.com");
    m.active_tab_mut().unwrap().load_traversal(back).unwrap();
    assert!(m.active_tab().unwrap().can_go_forward());

    let fwd = m.active_tab_mut().unwrap().go_forward().unwrap();
    assert_eq!(fwd, "https://c.com");
    m.active_tab_mut().unwrap().load_traversal(fwd).unwrap();
    assert_eq!(m.active_tab().unwrap().url, "https://c.com");
    assert!(!m.active_tab().unwrap().can_go_forward());
}

#[test]
fn pinned_close_returns_pinned_error() {
    let mut m = manager();
    m.new_tab_mock(Some("https://a.com".to_owned())).unwrap();
    m.toggle_pin(0).unwrap();
    let id = m.active_tab().unwrap().id;
    match m.close_tab(id) {
        Err(TabError::PinnedTab { id: got }) => assert_eq!(got, id),
        other => panic!("expected PinnedTab, got {other:?}"),
    }
    assert_eq!(m.len(), 1, "pinned tab survives normal close");
}

#[test]
fn close_never_fabricates_webviews_for_discarded() {
    let mut m = manager();
    m.new_tab_mock(Some("https://a.com".to_owned())).unwrap();
    m.new_tab_mock(Some("https://b.com".to_owned())).unwrap();
    // Discard the inactive tab, then close the active one.
    m.discard_tab(0).unwrap();
    assert!(m.iter().next().unwrap().is_discarded());
    let active_id = m.active_id().unwrap();
    m.close_tab(active_id).unwrap();
    let tab = m.active_tab().unwrap();
    assert!(tab.is_discarded(), "discarded stays discarded after close");
    assert!(tab.webview.is_none(), "no fabricated webview");
}

#[test]
fn session_roundtrip_restores_metadata_as_discarded() {
    let mut m = manager();
    m.new_tab_mock(Some("https://a.com".to_owned())).unwrap();
    m.new_tab_mock(Some("https://b.com".to_owned())).unwrap();
    m.toggle_pin(1).unwrap();
    let dir = std::env::temp_dir().join(format!(
        "ferrite-session-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session.json");
    m.save_to_file(&path).unwrap();
    assert!(path.exists());

    let mut m2 = manager();
    let restored = m2.load_from_file(&path);
    assert_eq!(restored, 2);
    assert!(m2.iter().all(|t| t.is_discarded()));
    assert!(m2.iter().all(|t| t.webview.is_none()));
    let urls: Vec<_> = m2.iter().map(|t| t.url.clone()).collect();
    assert!(urls.contains(&"https://a.com".to_owned()));
    assert!(urls.contains(&"https://b.com".to_owned()));
    assert!(m2.iter().any(|t| t.is_pinned));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn session_load_rejects_corrupt_file() {
    let mut m = manager();
    let dir = std::env::temp_dir().join(format!(
        "ferrite-session-bad-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session.json");
    std::fs::write(&path, b"{not json").unwrap();
    assert_eq!(m.load_from_file(&path), 0);
    assert!(m.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reload_preserves_history_and_stop_clears_loading() {
    let mut m = manager();
    m.new_tab_mock(Some("https://a.com".to_owned())).unwrap();
    let tab = m.active_tab_mut().unwrap();
    tab.load_url("https://b.com".to_owned()).unwrap();
    assert!(tab.can_go_back());

    tab.reload().unwrap();
    assert!(tab.is_loading);
    assert!(tab.can_go_back(), "reload must not clear history");
    assert_eq!(tab.url, "https://b.com");

    tab.stop().unwrap();
    assert!(!tab.is_loading);
    assert!(tab.can_go_back(), "stop must not clear history");
}
