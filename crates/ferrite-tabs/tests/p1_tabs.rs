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
    // String API: the shell owns bytes (encryption, atomic write), so the
    // manager round-trips documents, never files.
    let json = m.save_json().unwrap();
    assert!(json.contains("https://a.com"));

    let mut m2 = manager();
    let restored = m2.load_json(&json);
    assert_eq!(restored, 2);
    assert!(m2.iter().all(|t| t.is_discarded()));
    assert!(m2.iter().all(|t| t.webview.is_none()));
    let urls: Vec<_> = m2.iter().map(|t| t.url.clone()).collect();
    assert!(urls.contains(&"https://a.com".to_owned()));
    assert!(urls.contains(&"https://b.com".to_owned()));
    assert!(m2.iter().any(|t| t.is_pinned));
}

#[test]
fn session_load_rejects_corrupt_file() {
    let mut m = manager();
    assert_eq!(m.load_json("{not json"), 0);
    assert!(m.is_empty());
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

#[test]
fn structural_change_marks_dirty_and_save_stamps_time() {
    let mut m = manager();
    assert!(!m.take_dirty(), "fresh manager is clean");
    m.new_tab_mock(Some("https://a.com".to_owned())).unwrap();
    assert!(
        m.take_dirty(),
        "open marks dirty for the fast-path autosave"
    );
    assert!(
        !m.take_dirty(),
        "take clears so quiet browsers stop writing"
    );
    m.mark_dirty();
    assert!(m.take_dirty());

    let raw = m.save_json().unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(v.get("last_saved_at").and_then(|n| n.as_i64()).unwrap_or(0) > 0);

    // Old documents without the field still load (serde default).
    let legacy = r#"{"version":1,"active":0,"tabs":[{"url":"https://old.com","title":"old","is_pinned":false}]}"#;
    let mut m2 = manager();
    assert_eq!(m2.load_json(legacy), 1);
}

#[test]
fn show_only_active_reports_one_shown_rest_hidden() {
    let mut m = manager();
    m.new_tab_mock(Some("https://a.com".to_owned())).unwrap();
    m.new_tab_mock(Some("https://b.com".to_owned())).unwrap();
    assert_eq!(m.len(), 2);
    // Second tab is active; first was suspended on creation.
    assert_eq!(m.show_only_active(), (1, 1));
    m.switch_tab_by_index(0).unwrap();
    assert_eq!(m.active_tab().unwrap().url, "https://a.com");
    assert_eq!(m.show_only_active(), (1, 1));
}

#[test]
fn duplicate_mock_keeps_history_flags_and_suppresses_re_record() {
    let mut m = manager();
    m.new_tab_mock(Some("https://a.com".to_owned())).unwrap();
    m.new_tab_mock(Some("https://b.com".to_owned())).unwrap();
    let src = m.active_id().unwrap();
    m.active_tab_mut()
        .unwrap()
        .load_url("https://c.com".to_owned())
        .unwrap();
    m.toggle_mute(1).unwrap();
    m.toggle_pin(1).unwrap();
    let dup = m.duplicate_tab_mock(src).unwrap();
    assert_ne!(dup, src);
    assert_eq!(m.len(), 3);
    let tab = m.get_tab(dup).unwrap();
    assert_eq!(tab.url, "https://c.com");
    assert!(tab.can_go_back(), "back stack survives duplication");
    assert!(tab.is_muted);
    assert!(
        !tab.is_pinned,
        "duplicates land unpinned (pinned-left invariant)"
    );
    assert!(tab.suppress_next_history, "initial load must not re-record");
    assert!(tab.webview.is_some());
    assert!(m.get_tab(src).unwrap().is_private == tab.is_private);
    assert!(m.take_dirty(), "duplicate marks dirty like open");
}

#[test]
fn reorder_clamps_pinned_block_left() {
    let mut m = manager();
    m.new_tab_mock(Some("https://a.com".to_owned())).unwrap();
    m.new_tab_mock(Some("https://b.com".to_owned())).unwrap();
    m.new_tab_mock(Some("https://c.com".to_owned())).unwrap();
    m.new_tab_mock(Some("https://d.com".to_owned())).unwrap();
    m.toggle_pin(0).unwrap();
    // Unpinned tab cannot enter the pinned run: clamps to index 1.
    m.reorder(3, 0).unwrap();
    let urls: Vec<_> = m.iter().map(|t| t.url.clone()).collect();
    assert_eq!(
        urls,
        vec![
            "https://a.com",
            "https://d.com",
            "https://b.com",
            "https://c.com"
        ]
    );
    assert!(m.iter().next().unwrap().is_pinned);
    // Pinned tab cannot leave the run: clamps back, order untouched.
    m.reorder(0, 3).unwrap();
    let urls: Vec<_> = m.iter().map(|t| t.url.clone()).collect();
    assert_eq!(
        urls,
        vec![
            "https://a.com",
            "https://d.com",
            "https://b.com",
            "https://c.com"
        ]
    );
    // Plain moves inside the unpinned zone still work.
    m.reorder(3, 1).unwrap();
    let urls: Vec<_> = m.iter().map(|t| t.url.clone()).collect();
    assert_eq!(
        urls,
        vec![
            "https://a.com",
            "https://c.com",
            "https://d.com",
            "https://b.com"
        ]
    );
}

#[test]
fn windowless_switch_onto_discarded_reactivates_mock_lifecycle() {
    // Pinned contract (see also v12 check9/check12): headless switches flip
    // the mock lifecycle back to Active; only `switch_tab_with_window`
    // rebuilds real webviews, and production always routes through it.
    let mut m = manager();
    m.new_tab_mock(Some("https://a.com".to_owned())).unwrap();
    m.new_tab_mock(Some("https://b.com".to_owned())).unwrap();
    m.discard_tab(0).unwrap();
    assert!(m.iter().next().unwrap().is_discarded());
    m.switch_tab_by_index(0).unwrap();
    let tab = m.iter().next().unwrap();
    assert_eq!(tab.lifecycle, ferrite_tabs::TabLifecycle::Active);
}

#[test]
fn groups_assign_persist_and_prune() {
    let mut m = manager();
    m.new_tab_mock(Some("https://a.com".to_owned())).unwrap();
    m.new_tab_mock(Some("https://b.com".to_owned())).unwrap();
    m.new_tab_mock(Some("https://c.com".to_owned())).unwrap();
    let ids: Vec<_> = m.iter().map(|t| t.id).collect();
    // Empty and oversize names are rejected, never stored.
    assert!(m.create_group("").is_err());
    assert!(m.create_group(&"x".repeat(65)).is_err());
    let gid = m.create_group("Work").unwrap();
    m.set_tab_group(ids[0], Some(gid)).unwrap();
    m.set_tab_group(ids[1], Some(gid)).unwrap();
    assert_eq!(m.tabs_in_group(gid).len(), 2);
    // Unknown group and bad color fail loudly.
    assert!(m.set_tab_group(ids[2], Some(uuid::Uuid::new_v4())).is_err());
    assert!(m.set_group_color(gid, 99).is_err());
    m.set_group_color(gid, 1).unwrap();
    assert_eq!(m.find_group(gid).unwrap().color, 1);
    m.rename_group(gid, "Play").unwrap();
    assert_eq!(m.find_group(gid).unwrap().name, "Play");
    // Pinned tabs cannot join groups.
    m.toggle_pin(2).unwrap();
    assert!(m.set_tab_group(ids[2], Some(gid)).is_err());
    // Collapse is state only: members stay loaded.
    m.set_group_collapsed(gid, true).unwrap();
    assert!(m.find_group(gid).unwrap().collapsed);
    // Session round-trips groups with members; the pinned outsider stays out.
    let json = m.save_json().unwrap();
    let mut m2 = manager();
    assert_eq!(m2.load_json(&json), 3);
    assert_eq!(m2.groups().len(), 1);
    assert_eq!(m2.find_group(gid).unwrap().name, "Play");
    let urls: Vec<_> = m2
        .iter()
        .filter(|t| t.group_id == Some(gid))
        .map(|t| t.url.clone())
        .collect();
    assert_eq!(urls, vec!["https://a.com", "https://b.com"]);
    // Closing the last member dissolves the group (no empty groups).
    let members = m2.tabs_in_group(gid);
    for id in members {
        m2.close_tab(id).unwrap();
    }
    assert!(m2.groups().is_empty());
}

#[test]
fn group_tabs_close_and_move_as_a_unit() {
    let mut m = manager();
    for u in [
        "https://a.com",
        "https://b.com",
        "https://c.com",
        "https://d.com",
    ] {
        m.new_tab_mock(Some(u.to_owned())).unwrap();
    }
    let ids: Vec<_> = m.iter().map(|t| t.id).collect();
    // Bulk create skips unknown ids, still groups the rest.
    let (gid, n) = m
        .group_tabs(&[ids[1], ids[2], uuid::Uuid::new_v4()], "Set", 2, false)
        .unwrap();
    assert_eq!(n, 2);
    // Move the run to the front as one unit.
    m.move_group(gid, 0).unwrap();
    let urls: Vec<_> = m.iter().map(|t| t.url.clone()).collect();
    assert_eq!(
        urls,
        vec![
            "https://b.com",
            "https://c.com",
            "https://a.com",
            "https://d.com"
        ]
    );
    // Close the group: members die, outsiders survive, group prunes.
    let (closed, kept) = m.close_group(gid).unwrap();
    assert_eq!((closed, kept), (2, 0));
    assert_eq!(m.len(), 2);
    assert!(m.groups().is_empty());
    assert!(m.close_group(gid).is_err());
}

#[test]
fn move_tabs_keeps_block_order_and_skips_pinned() {
    let mut m = manager();
    for u in [
        "https://a.com",
        "https://b.com",
        "https://c.com",
        "https://d.com",
    ] {
        m.new_tab_mock(Some(u.to_owned())).unwrap();
    }
    let ids: Vec<_> = m.iter().map(|t| t.id).collect();
    // Block b,c to the front: relative order preserved.
    let (moved, kept) = m.move_tabs(&[ids[2], ids[1]], 0);
    assert_eq!((moved, kept), (2, 0));
    let urls: Vec<_> = m.iter().map(|t| t.url.clone()).collect();
    assert_eq!(
        urls,
        vec![
            "https://b.com",
            "https://c.com",
            "https://a.com",
            "https://d.com"
        ]
    );
    // Pinned members stay: skipped and counted, never dragged along.
    m.toggle_pin(2).unwrap(); // pins https://a.com
    let ids: Vec<_> = m.iter().map(|t| t.id).collect();
    let (moved, kept) = m.move_tabs(&ids, 0);
    assert_eq!((moved, kept), (3, 1));
    let urls: Vec<_> = m.iter().map(|t| t.url.clone()).collect();
    assert_eq!(
        urls,
        vec![
            "https://b.com",
            "https://c.com",
            "https://d.com",
            "https://a.com"
        ]
    );
    assert!(m.iter().nth(3).unwrap().is_pinned);
}

#[test]
fn workspaces_park_switch_and_persist() {
    let mut m = manager();
    m.new_tab_mock(Some("https://a.com".to_owned())).unwrap();
    m.new_tab_mock(Some("https://b.com".to_owned())).unwrap();
    assert!(m.create_workspace("").is_err());
    let wid = m.create_workspace("Research").unwrap();
    // Create switches in: live tree is the fresh workspace (empty).
    assert_eq!(m.active_space_name().as_deref(), Some("Research"));
    assert_eq!(m.len(), 0);
    m.new_tab_mock(Some("https://w.com".to_owned())).unwrap();
    // Back to default: default tree restores intact.
    m.switch_default().unwrap();
    assert_eq!(m.active_space_name(), None);
    let urls: Vec<_> = m.iter().map(|t| t.url.clone()).collect();
    assert_eq!(urls, vec!["https://a.com", "https://b.com"]);
    // Back to Research: its tab survived the park round-trip.
    m.switch_workspace(wid).unwrap();
    let urls: Vec<_> = m.iter().map(|t| t.url.clone()).collect();
    assert_eq!(urls, vec!["https://w.com"]);
    // Listing counts live for the active space.
    let list = m.workspace_list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0, wid);
    assert_eq!(list[0].2, 1);
    m.rename_workspace(wid, "Deep Research").unwrap();
    assert_eq!(m.active_space_name().as_deref(), Some("Deep Research"));
    // Persist: default + space + active marker round-trip.
    let json = m.save_json().unwrap();
    let mut m2 = manager();
    let n = m2.load_json(&json);
    assert!(n >= 3, "default tabs plus workspace tab restore");
    assert_eq!(m2.active_space_name().as_deref(), Some("Deep Research"));
    let urls: Vec<_> = m2.iter().map(|t| t.url.clone()).collect();
    assert_eq!(urls, vec!["https://w.com"]);
    // Delete the active workspace: returns to default, drops traced.
    let dropped = m2.delete_workspace(wid).unwrap();
    assert_eq!(m2.active_space_name(), None);
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].0, "https://w.com");
    let urls: Vec<_> = m2.iter().map(|t| t.url.clone()).collect();
    assert_eq!(urls, vec!["https://a.com", "https://b.com"]);
    assert!(m2.workspace_list().is_empty());
    assert!(m2.delete_workspace(wid).is_err());
    assert!(m2.switch_workspace(wid).is_err());
}

#[test]
fn sleep_exemptions_cover_signals_pins_and_listed_hosts() {
    let mut m = manager();
    m.new_tab_mock(Some("https://music.example.com".to_owned()))
        .unwrap();
    m.new_tab_mock(Some("https://docs.example.org".to_owned()))
        .unwrap();
    let music = m.iter().next().unwrap().id;
    // Listed hosts (suffix match) exempt; others are not.
    m.set_sleep_exceptions(vec!["example.com".to_owned()]);
    assert_eq!(m.sleep_exceptions(), &["example.com".to_owned()]);
    let midx = m.iter().position(|t| t.id == music).unwrap();
    assert!(m.sleep_exempt(m.iter().nth(midx).unwrap()));
    let docs = m.iter().nth(1).unwrap().id;
    let didx = m.iter().position(|t| t.id == docs).unwrap();
    assert!(!m.sleep_exempt(m.iter().nth(didx).unwrap()));
    // Normalization: case, leading dot, blanks, dupes collapse.
    m.set_sleep_exceptions(vec![
        "  .Example.ORG ".to_owned(),
        "example.org".to_owned(),
        "".to_owned(),
    ]);
    assert_eq!(m.sleep_exceptions(), &["example.org".to_owned()]);
    assert!(m.sleep_exempt(m.iter().nth(didx).unwrap()));
    // Pinned tabs exempt at normal pressure, not under extreme pressure.
    // (List cleared first: the host exemption would mask the pin logic.)
    m.set_sleep_exceptions(Vec::new());
    m.toggle_pin(didx).unwrap();
    m.set_memory_pressure(0.5);
    assert!(m.sleep_exempt(m.iter().nth(didx).unwrap()));
    m.set_memory_pressure(0.95);
    assert!(!m.sleep_exempt(m.iter().nth(didx).unwrap()));
    // Timeout is settable (shell owns the minutes; manager owns duration).
    // Mid pressure selects the configured timeout exactly (no floor).
    m.set_memory_pressure(0.5);
    m.set_idle_timeout(std::time::Duration::from_secs(5 * 60));
    assert_eq!(
        m.effective_idle_timeout(),
        std::time::Duration::from_secs(5 * 60)
    );
}

#[test]
fn downloading_tabs_never_auto_sleep() {
    let mut m = manager();
    m.new_tab_mock(Some("https://dl.test/big.iso".to_owned()))
        .unwrap();
    m.active_tab_mut().unwrap().has_active_download = true;
    // Auto-sleep refuses at any pressure, exempt signals or not.
    m.set_memory_pressure(1.0);
    assert!(m.sleep_exempt(m.active_tab().unwrap()));
    // Flag cleared (transfer done): pressure rules again.
    m.active_tab_mut().unwrap().has_active_download = false;
    m.set_memory_pressure(0.5);
    assert!(!m.sleep_exempt(m.active_tab().unwrap()));
}

#[test]
fn sleep_reasons_name_the_blocker_and_discards_count() {
    let mut m = manager();
    m.new_tab_mock(Some("https://a.test/".to_owned())).unwrap();
    m.new_tab_mock(Some("https://b.test/".to_owned())).unwrap();
    // Plain background tab: discardable, no reason.
    assert_eq!(m.sleep_exempt_reason(m.iter().next().unwrap()), None);
    // Audio pins a reason.
    m.iter_mut().next().unwrap().is_playing_audio = true;
    assert_eq!(
        m.sleep_exempt_reason(m.iter().next().unwrap()),
        Some("playing audio")
    );
    assert!(m.sleep_exempt(m.iter().next().unwrap()));
    m.iter_mut().next().unwrap().is_playing_audio = false;
    // Listed site pins a reason.
    m.set_sleep_exceptions(vec!["a.test".to_owned()]);
    assert_eq!(
        m.sleep_exempt_reason(m.iter().next().unwrap()),
        Some("listed site")
    );
    m.set_sleep_exceptions(Vec::new());
    // Discard counts per unload, idempotent on repeat calls.
    m.discard_tab(0).unwrap();
    m.discard_tab(0).unwrap();
    assert_eq!(m.iter().next().unwrap().discard_count, 1);
    assert!(m.iter().next().unwrap().is_discarded());
}

#[test]
fn session_round_trips_history_scroll_and_groups() {
    let mut m = manager();
    m.new_tab_mock(Some("https://a.com".to_owned())).unwrap();
    let tab = m.active_tab_mut().unwrap();
    tab.load_url("https://b.com".to_owned()).unwrap();
    tab.load_url("https://c.com".to_owned()).unwrap();
    tab.scroll_position = (0.0, 128.0);
    let gid = m.create_group("Work").unwrap();
    let id = m.active_id().unwrap();
    m.set_tab_group(id, Some(gid)).unwrap();
    m.new_tab_mock(Some("https://fresh.com".to_owned()))
        .unwrap();
    let json = m.save_json().unwrap();
    let mut m2 = manager();
    assert_eq!(m2.load_json(&json), 2);
    // History survives: Back works on the restored tab.
    let tab = m2.iter().find(|t| t.url == "https://c.com").unwrap();
    assert!(tab.can_go_back());
    assert_eq!(tab.history.back_len(), 2);
    assert_eq!(tab.scroll_position, (0.0, 128.0));
    assert_eq!(tab.group_id, Some(gid));
    assert_eq!(m2.groups().len(), 1);
    // Hostile-but-parseable entries do not survive the round trip
    // (empties/overlong dropped, wild scroll clamped). Truly corrupt
    // JSON still takes the quarantine path, never a partial load.
    let mut m3 = manager();
    let mut doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    doc["tabs"][0]["history"]["back_stack"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::Value::String("z".repeat(5000)));
    doc["tabs"][0]["history"]["back_stack"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::Value::String(String::new()));
    doc["tabs"][0]["scroll"] = serde_json::json!([0.0, 1e30]);
    m3.load_json(&serde_json::to_string(&doc).unwrap());
    let tab = m3.iter().find(|t| t.url == "https://c.com").unwrap();
    assert_eq!(tab.history.back_len(), 2);
    assert_eq!(tab.scroll_position, (0.0, 1e7));
}
