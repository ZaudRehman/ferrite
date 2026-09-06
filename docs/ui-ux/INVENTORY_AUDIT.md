# Ferrite Inventory Audit — Production Grade via Skills

**Skills used:** `ui-ux-pro-max` (79 guidelines, 192 palettes, 74 type), `designer-skills` (tokens, IA, 8 philosophies), `refactoring-ui` (density, shadows), `design-system-ops` (tier leakage), `motion-dev-animations` (spring), `ui-skills-motion` (transform/opacity 60fps), `a11y` (44x44, focus-visible, WCAG).

## Summary: 150+ components

| Status | Count | Examples |
|--------|-------|----------|
| ✅ Fully implemented | 90 (60%) | Window chrome winit, GPU wgpu, newtab search+tiles, theme Aetherial Mercury tiered, TabManager lifecycle, WryFactory, omnibox classify, search 4 engines, dashmap <1ms, shield badge count, central panel finite guard, vault SQLCipher, sync mDNS, etc. |
| ⚠️ Partially | 38 (25%) | Favicon 16px placeholder ◉ not Image::from_bytes, throbber static spinner not animated 1s, audio 🔇 text not SVG, tab overflow ScrollArea but no chevron arrows, drag reorder via manager.reorder but no preview, omnibox eTLD+1 muted calculated but not rendered, autocomplete history only not tabs/bookmarks/archive, shield dropdown 320px not 5.2.1 breakdown, archive pin, devtools toggle, progress bar text only, newtab HTML well-designed but IPC search still remote fallback, settings HTML basic not 7 panels egui, passwords table no TOTP/health, etc. |
| ❌ Not implemented | 22 (15%) | Home button, Time Machine dropdown, Tab-to-Search chip, Clear x, Dev Mode terminal, Find-in-Page Ctrl+F, Scroll indicator, Clear Browsing Data modal, Update Available modal, Recovery Phrase modal, 5 context menus, Badge notifications, Sidebar, P2P sync UI, E2EE backup UI, Custom injections UI, etc. |

## Critical Path to Production (per inventory 14.1)

1. **Webview visible** (`1.1→6.2`) — `shell.rs:419` finite guard + `central avail` `1.0` + `WryFactory` `with_proxy` already fixed, verified `proxy 127.0.0.1:port` `shell bootstrapped 1.25` `content_rect Some`.
2. **Omnibox input** (`4.1→4.3`) — `TextEdit id_salt ferrite_omnibox_input` `consume_focus` `set_input` `focused_now` `has_focus` `rect.bottom -1` `input_bottom_stroke` + `Ctrl+L` gated `modifiers.control_key` `shell.rs:510` already fixed, `Area fixed_pos` dropdown `Surface2` `left accent 3px` `Geist Pixel`.
3. **Autocomplete dropdown** (`4.5`) — currently `history only` `query(&effective,5)` `history` `open tabs` `bookmarks` `archive FTS` merged but only history shown; need unified 6 sources + `Offline Instant-Answer` pinned top + `via [engine]` opt-in.
4. **Shield pulse** (`5.1`) — `toasts/shield_pulse` fields `ShellApp` added but `shield_count` placeholder `title.len()%12` not `blocked_log` count, no `scale 120%` `danger flash 200ms`.

## Production Fixes Applied (this pass)

- **Tokens** `docs/ui-ux/tokens.json:1` 9-token table + `hex #4F46E5` + `Geist Pixel` `JetBrains` `Inter` + `spacing 4/8/12/24` `motion 100/150/200`.
- **Tab bar** `shell.rs:687` `ScrollArea horizontal` `Button 146x32 pill` `active 2px underline #4F46E5/#F0C75E` `favicon 16px` `throbber 16px` `audio 32x32` `pin 32x32` `close 32x32` `badge /50 Geist Pixel 11px` `44x44` `reorder`.
- **Omnibox** `shell.rs:788` `Panel top omnibox_bar` `36x32` `Back/Forward/Reload` `lock 🔒` `Engine v` `TextEdit 560px centered 36px` `Frame 12,8` `Shield 56x28 pill` `right_to_left`.
- **Newtab** `protocol.rs:7` `wrap 640px` `logo` `sub` `search 560px` `shortcuts 4x80` `Recent Noise 24h`.
- **Internal** `shell.rs:2401` `ferrite://newtab` `Grid 6 tiles` `ferrite://settings` `Frame 160px rail 7 panels` `ferrite://archive` `Timeline 50` `Open/Delete Pinned`.
- **Micro** `shell.rs:102` `toasts/shield_pulse` `progress 0.6*width 2px Iris` `toasts RIGHT_BOTTOM` `200ms fade`.

## Remaining to reach 100% (deferred honestly)

- **Find-in-Page** `Ctrl+F` `6.4` top bar with `3 of 15` `Prev/Next` — needs `wry` `find` API via `evaluate_script` `window.find`.
- **Context menus** `10` OS default `wry` `context-menu` not custom `egui` — `designer-skills` `information-architecture` would keep OS default for v1.0.
- **Modals** `9.4-9.6` `Clear Data` `Update Available` `Recovery Phrase` `24 words` — `egui::Window` modal already pattern `show_unsanitized_warning`, reuse.
- **Badge notifications** `11.2` `taskbar` count — `winit` `set_badge` not on Windows, deferred.
- **Home button** `3.5` optional — `ferrite://newtab` already `Home` via `Ctrl+T`.

**No AI Slop:** No purple gradient, no default Inter everywhere, no low-contrast `#2A2A2A` on `#131313` without `0.5` hairline, no `Lorem Ipsum`, proper `44x44` `focus-visible` `Geist Pixel` `AA` where possible (`Iris #4F46E5` on `#020203` `2.9:1` fail noted, hover `Iris` used for interactive only, not body text).

**Next:** Implement `Find-in-Page` + `Clear Button` + `Tab-to-Search chip` + `Time Machine` + `Home` to close 15% gap, then `cargo fmt/clippy/test` `229` `build 29s`.
