use ferrite_tabs::{MockFactory, TabManager};
use std::rc::Rc;

#[test]
fn twenty_tabs_stability() {
    let mut m = TabManager::new(Rc::new(MockFactory));
    let mut ids = Vec::new();
    for i in 0..20 {
        let url = format!("https://example.com/{}", i);
        let id = m.new_tab_mock(Some(url)).unwrap();
        ids.push(id);
        assert_eq!(m.len(), i + 1);
        assert_eq!(m.active_id(), Some(id));
    }
    // Switch through all
    for id in &ids {
        m.switch_tab(*id).unwrap();
        assert_eq!(m.active_id(), Some(*id));
    }
    // Close 10 from middle
    for id in ids.iter().take(10) {
        m.close_tab(*id).unwrap();
    }
    assert_eq!(m.len(), 10);
    // Close remaining
    while let Some(id) = m.active_id() {
        m.close_tab(id).unwrap();
    }
    assert!(m.is_empty());
    assert_eq!(m.active_id(), None);
}

#[test]
fn twenty_tabs_no_deadlock_ui_thread() {
    let mut m = TabManager::new(Rc::new(MockFactory));
    for i in 0..20 {
        m.new_tab_mock(Some(format!("https://reddit.com/r/{}", i)))
            .unwrap();
    }
    // Simulate rapid resize -> bounds updates for active tab only
    for _ in 0..100 {
        let _ = m.update_active_bounds(wry::Rect {
            position: wry::dpi::Position::Logical(wry::dpi::LogicalPosition::new(0.0, 40.0)),
            size: wry::dpi::Size::Logical(wry::dpi::LogicalSize::new(1280.0, 760.0)),
        });
    }
    assert_eq!(m.len(), 20);
}
