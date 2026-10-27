//! DESIGN packs as CSS vars. Manual copy from `DESIGN/*.md` frontmatter.
//! `--f-*` names stay compatible with shell theme output.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemePack {
    VolcanicObsidian,
    Tenebra,
    AetherialMercury,
    NebulaArchive,
    DeepSpace,
    Browserbase,
    PrismVoid,
    SolarSilk,
}

impl ThemePack {
    pub const ALL: [Self; 8] = [
        Self::VolcanicObsidian,
        Self::Tenebra,
        Self::AetherialMercury,
        Self::NebulaArchive,
        Self::DeepSpace,
        Self::Browserbase,
        Self::PrismVoid,
        Self::SolarSilk,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::VolcanicObsidian => "volcanic-obsidian",
            Self::Tenebra => "tenebra",
            Self::AetherialMercury => "aetherial-mercury",
            Self::NebulaArchive => "nebula-archive",
            Self::DeepSpace => "deep-space",
            Self::Browserbase => "browserbase",
            Self::PrismVoid => "prism-void",
            Self::SolarSilk => "solar-silk",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::VolcanicObsidian => "Volcanic Obsidian",
            Self::Tenebra => "Tenebra",
            Self::AetherialMercury => "Aetherial Mercury",
            Self::NebulaArchive => "Nebula Archive",
            Self::DeepSpace => "Deep Space",
            Self::Browserbase => "Browserbase",
            Self::PrismVoid => "Prism Void",
            Self::SolarSilk => "Solar Silk",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|p| p.name() == name)
    }

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    pub fn accent(self) -> &'static str {
        match self {
            Self::VolcanicObsidian => "#ff8c00",
            Self::Tenebra => "#ffc670",
            Self::AetherialMercury => "#c3c0ff",
            Self::NebulaArchive => "#f0c75e",
            Self::DeepSpace => "#b7c8e1",
            Self::Browserbase => "#000000",
            Self::PrismVoid => "#000000",
            Self::SolarSilk => "#843728",
        }
    }

    /// Frame background for the OS caption so it matches the pack.
    pub fn frame_bg(self) -> &'static str {
        match self {
            Self::VolcanicObsidian => "#131315",
            Self::Tenebra => "#0d0a06",
            Self::AetherialMercury => "#131313",
            Self::NebulaArchive => "#16130c",
            Self::DeepSpace => "#0c1324",
            Self::Browserbase => "#ffffff",
            Self::PrismVoid => "#fcf9f8",
            Self::SolarSilk => "#fef8f6",
        }
    }

    pub fn frame_dark(self) -> bool {
        !matches!(self, Self::Browserbase | Self::PrismVoid | Self::SolarSilk)
    }

    pub fn css(self) -> &'static str {
        match self {
            Self::VolcanicObsidian => VOLCANIC_OBSIDIAN_CSS,
            Self::Tenebra => TENEBRA_CSS,
            Self::AetherialMercury => AETHERIAL_MERCURY_CSS,
            Self::NebulaArchive => NEBULA_ARCHIVE_CSS,
            Self::DeepSpace => DEEP_SPACE_CSS,
            Self::Browserbase => BROWSERBASE_CSS,
            Self::PrismVoid => PRISM_VOID_CSS,
            Self::SolarSilk => SOLAR_SILK_CSS,
        }
    }
}

/// Bundled shell template. Marker duplicated here to avoid a tabs
/// dependency cycle.
pub const CHROME_TEMPLATE: &str = include_str!("../assets/chrome.html");

/// Bundled mobile shell template: bottom toolbar, tab switcher cards,
/// omnibox sheet, bottom-sheet menu: same IPC contract as desktop
/// (same `__ferriteChrome` push, same `{cmd}` posts, same list hooks),
/// so the shell needs zero changes to drive either layout.
pub const CHROME_MOBILE_TEMPLATE: &str = include_str!("../assets/chrome_mobile.html");

/// Template with `pack` tokens inlined. Used by tests and previews.
pub fn chrome_html(pack: ThemePack) -> String {
    CHROME_TEMPLATE.replacen("/*__FERRITE_THEME__*/", pack.css(), 1)
}

/// Mobile template with `pack` tokens inlined. The future mobile shell
/// loads this instead of [`chrome_html`]; everything else (state shape,
/// commands, hooks) is shared by construction.
pub fn chrome_mobile_html(pack: ThemePack) -> String {
    CHROME_MOBILE_TEMPLATE.replacen("/*__FERRITE_THEME__*/", pack.css(), 1)
}

pub const VOLCANIC_OBSIDIAN_CSS: &str = concat!(
    ":root{",
    /* --f-* compat (ferrite:// pages) */
    "--f-bg:#131315;--f-surface-1:#1c1b1e;--f-surface-2:#201f22;",
    "--f-fg:#e5e1e4;--f-muted:#c8c6c8;--f-faint:#a48c7a;",
    "--f-accent:#ff8c00;--f-accent-primary:#ffb77d;",
    "--f-danger:#ffb4ab;--f-warning:#ffb77d;--f-archive:#f0c75e;",
    "--f-border:#564334;",
    "--f-font-ui:'Space Grotesk',system-ui,sans-serif;",
    "--f-font-mono:'Space Grotesk',ui-monospace,monospace;",
    /* full Volcanic surface scale */
    "--vo-surface:#131315;--vo-lowest:#0e0e10;--vo-low:#1c1b1e;",
    "--vo-container:#201f22;--vo-high:#2a2a2c;--vo-highest:#353437;",
    "--vo-bright:#39393b;--vo-on-surface:#e5e1e4;--vo-on-variant:#ddc1ae;",
    "--vo-outline:#a48c7a;--vo-outline-variant:#564334;",
    "--vo-primary:#ffb77d;--vo-primary-container:#ff8c00;",
    "--vo-on-primary:#4d2600;--vo-on-primary-container:#623200;",
    "--vo-secondary:#c8c6c8;--vo-secondary-container:#474649;",
    "--vo-tertiary:#85cfff;--vo-tertiary-container:#00b5fc;",
    "--vo-error:#ffb4ab;--vo-error-container:#93000a;",
    "--vo-border:#2d2d30;--vo-border-strong:#444444;",
    "--vo-radius:2px;--vo-radius-modal:4px;",
    "--vo-unit:4px;--vo-gutter:24px;--vo-container-max:1440px;",
    "--vo-font-display:'Space Grotesk',system-ui,sans-serif;",
    "--pk-tab-radius:2px;--pk-pill-radius:16px;--pk-pop-radius:4px;--pk-pop-blur:0px;",
    "--pk-card-radius:8px;--pk-ctl-radius:6px;",
    "}",
    /* chrome base rules, flat, borders over shadows */
    ".vo-app{background:var(--vo-surface);color:var(--vo-on-surface);",
    "font-family:var(--vo-font-display);}",
    ".vo-panel{background:var(--vo-low);border:1px solid var(--vo-border);",
    "border-radius:var(--vo-radius);}",
    ".vo-btn-primary{background:var(--vo-primary-container);color:#000;",
    "border-radius:var(--vo-radius);border:none;}",
    ".vo-btn-secondary{background:transparent;color:var(--vo-primary);",
    "border:1px solid var(--vo-primary);border-radius:var(--vo-radius);}",
    ".vo-input{background:var(--vo-low);border:1px solid var(--vo-border);",
    "border-radius:var(--vo-radius);color:var(--vo-on-surface);}",
    ".vo-input:focus{border-color:var(--vo-primary-container);outline:none;}",
    ".vo-float{background:var(--vo-low);border:1px solid var(--vo-border-strong);",
    "border-radius:var(--vo-radius-modal);",
    "box-shadow:0 0 0 1px rgba(255,140,0,0.10);}",
    ".vo-active-bar{border-left:2px solid var(--vo-primary-container);}",
);

pub const TENEBRA_CSS: &str = concat!(
    ":root{--f-bg:#0d0a06;--f-surface-1:#120e08;--f-surface-2:#171209;",
    "--f-fg:#f0e7d5;--f-muted:#cdbfa4;--f-faint:#8f7c57;",
    "--f-accent:#ffc670;--f-accent-primary:#f4b762;",
    "--f-danger:#ffb4ab;--f-warning:#f4b762;--f-archive:#f0c75e;",
    "--f-border:#43381f;",
    "--f-font-ui:'Archivo',system-ui,sans-serif;",
    "--f-font-pixel:'IBM Plex Mono',ui-monospace,monospace;",
    "--f-font-mono:'IBM Plex Mono',ui-monospace,SFMono-Regular,monospace;",
    "--pk-tab-radius:0px;--pk-pill-radius:0px;--pk-pop-radius:0px;--pk-pop-blur:0px;",
    "--pk-card-radius:0px;--pk-ctl-radius:0px;}",
);

pub const AETHERIAL_MERCURY_CSS: &str = concat!(
    ":root{--f-bg:#131313;--f-surface-1:#1c1b1b;--f-surface-2:#201f1f;",
    "--f-fg:#e5e2e1;--f-muted:#c4c7c8;--f-faint:#8e9192;",
    "--f-accent:#c3c0ff;--f-accent-primary:#ffffff;",
    "--f-danger:#ffb4ab;--f-warning:#e2e2e2;--f-archive:#f0c75e;",
    "--f-border:#444748;",
    "--f-font-ui:'Plus Jakarta Sans',system-ui,sans-serif;",
    "--f-font-pixel:'JetBrains Mono',ui-monospace,monospace;",
    "--f-font-mono:'JetBrains Mono',ui-monospace,SFMono-Regular,monospace;",
    "--pk-tab-radius:0px;--pk-pill-radius:999px;--pk-pop-radius:0px;--pk-pop-blur:20px;",
    "--pk-card-radius:0px;--pk-ctl-radius:999px;}",
);

pub const NEBULA_ARCHIVE_CSS: &str = concat!(
    ":root{--f-bg:#16130c;--f-surface-1:#1f1b14;--f-surface-2:#231f18;",
    "--f-fg:#eae1d5;--f-muted:#d1c5b1;--f-faint:#99907d;",
    "--f-accent:#f0c75e;--f-accent-primary:#ffe5ad;",
    "--f-danger:#ffb4ab;--f-warning:#eac259;--f-archive:#f0c75e;",
    "--f-border:#4d4636;",
    "--f-font-ui:'Inter',system-ui,sans-serif;",
    "--f-font-pixel:'Inter',ui-monospace,monospace;",
    "--f-font-mono:'Inter',ui-monospace,SFMono-Regular,monospace;",
    "--pk-tab-radius:0px;--pk-pill-radius:0px;--pk-pop-radius:0px;--pk-pop-blur:12px;",
    "--pk-card-radius:0px;--pk-ctl-radius:0px;}",
);

pub const DEEP_SPACE_CSS: &str = concat!(
    ":root{--f-bg:#0c1324;--f-surface-1:#151b2d;--f-surface-2:#191f31;",
    "--f-fg:#dce1fb;--f-muted:#c5c6cb;--f-faint:#8e9195;",
    "--f-accent:#b7c8e1;--f-accent-primary:#ffffff;",
    "--f-danger:#ffb4ab;--f-warning:#dde3eb;--f-archive:#f0c75e;",
    "--f-border:#44474a;",
    "--f-font-ui:'Space Grotesk',system-ui,sans-serif;",
    "--f-font-pixel:'Geist',ui-monospace,monospace;",
    "--f-font-mono:'JetBrains Mono',ui-monospace,SFMono-Regular,monospace;",
    "--pk-tab-radius:4px;--pk-pill-radius:4px;--pk-pop-radius:8px;--pk-pop-blur:12px;",
    "--pk-card-radius:8px;--pk-ctl-radius:4px;}",
);

pub const BROWSERBASE_CSS: &str = concat!(
    ":root{--f-bg:#ffffff;--f-surface-1:#f8fafc;--f-surface-2:#e2e9f3;",
    "--f-fg:#000000;--f-muted:#686562;--f-faint:#c5d3e8;",
    "--f-accent:#000000;--f-accent-primary:#000000;",
    "--f-danger:#ba1a1a;--f-warning:#ff4500;--f-archive:#765a00;",
    "--f-border:#e2e9f3;",
    "--f-font-ui:'Inter',system-ui,sans-serif;",
    "--f-font-pixel:'JetBrains Mono',ui-monospace,monospace;",
    "--f-font-mono:'JetBrains Mono',ui-monospace,SFMono-Regular,monospace;",
    "--pk-tab-radius:4px;--pk-pill-radius:50px;--pk-pop-radius:4px;--pk-pop-blur:0px;",
    "--pk-card-radius:4px;--pk-ctl-radius:50px;}",
);

pub const PRISM_VOID_CSS: &str = concat!(
    ":root{--f-bg:#fcf9f8;--f-surface-1:#f6f3f2;--f-surface-2:#f0eded;",
    "--f-fg:#1b1c1c;--f-muted:#4c4546;--f-faint:#7e7576;",
    "--f-accent:#000000;--f-accent-primary:#000000;",
    "--f-danger:#ba1a1a;--f-warning:#5d5f5f;--f-archive:#765a00;",
    "--f-border:#cfc4c5;",
    "--f-font-ui:'Geist',system-ui,sans-serif;",
    "--f-font-pixel:ui-monospace,SFMono-Regular,monospace;",
    "--f-font-mono:ui-monospace,SFMono-Regular,monospace;",
    "--pk-tab-radius:0px;--pk-pill-radius:0px;--pk-pop-radius:0px;--pk-pop-blur:0px;",
    "--pk-card-radius:0px;--pk-ctl-radius:0px;}",
);

pub const SOLAR_SILK_CSS: &str = concat!(
    ":root{--f-bg:#fef8f6;--f-surface-1:#f8f2f0;--f-surface-2:#f2edeb;",
    "--f-fg:#1d1b1a;--f-muted:#55433f;--f-faint:#88726e;",
    "--f-accent:#843728;--f-accent-primary:#843728;",
    "--f-danger:#ba1a1a;--f-warning:#a24e3d;--f-archive:#765a00;",
    "--f-border:#dbc1bc;",
    "--f-font-ui:'DM Sans',system-ui,sans-serif;",
    "--f-font-pixel:'DM Sans',ui-monospace,monospace;",
    "--f-font-mono:ui-monospace,SFMono-Regular,monospace;",
    "--pk-tab-radius:4px;--pk-pill-radius:4px;--pk-pop-radius:8px;--pk-pop-blur:0px;",
    "--pk-card-radius:8px;--pk-ctl-radius:4px;}",
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volcanic_css_carries_signal_flare_and_geometry() {
        assert!(VOLCANIC_OBSIDIAN_CSS.contains("--vo-primary-container:#ff8c00"));
        assert!(VOLCANIC_OBSIDIAN_CSS.contains("--vo-radius:2px"));
        assert!(VOLCANIC_OBSIDIAN_CSS.contains("--vo-radius-modal:4px"));
        // No shadow-based elevation in v1 (borders + glow only).
        assert!(
            !VOLCANIC_OBSIDIAN_CSS.contains("box-shadow:0 4px"),
            "v1 depth is tonal, not shadows"
        );
    }

    #[test]
    fn compat_vars_cover_ferrite_pages() {
        for var in ["--f-bg:", "--f-fg:", "--f-accent:", "--f-border:"] {
            assert!(VOLCANIC_OBSIDIAN_CSS.contains(var), "missing {var}");
        }
    }

    #[test]
    fn chrome_html_inlines_tokens_and_stays_bundled() {
        // The template itself names no palette: all color arrives as vars.
        // (`#ffc670` appears once as the boot-state default accent and is
        // overwritten by the first push, so it is excluded here.)
        for hard in ["#131315", "#e5e1e4", "#1c1b1e"] {
            assert!(
                !CHROME_TEMPLATE.contains(hard),
                "hardcoded {hard} in template"
            );
        }
        assert!(
            CHROME_TEMPLATE.contains("var(--f-bg)"),
            "var-driven surfaces"
        );
        assert!(
            CHROME_TEMPLATE.contains("/*__FERRITE_THEME__*/"),
            "shared theme marker"
        );
        let html = chrome_html(ThemePack::VolcanicObsidian);
        assert!(
            html.contains("--vo-primary-container:#ff8c00"),
            "tokens inlined"
        );
        assert!(!html.contains("__FERRITE_THEME__"), "marker replaced");
        // The theme layer owns scrollbars app-wide. A chrome-local
        // `scrollbar-width` override would suppress the themed bar and bring
        // back WebView2's light classic one on the dark tab strip.
        assert!(
            !CHROME_TEMPLATE.contains("scrollbar-width"),
            "theme owns scrollbars"
        );
        // Same trust class as privileged pages: inline scripts only, no
        // remote fetches, typed commands over window.ipc.
        assert!(html.contains("default-src 'none'"), "CSP locked");
        assert!(
            html.contains("script-src 'unsafe-inline'"),
            "chrome JS is inline"
        );
        assert!(html.contains("__ferriteChrome"), "renderer hook");
        assert!(html.contains("__ferriteTheme"), "pack-switch hook");
        assert!(html.contains("request_state"), "bootstraps from shell push");
        assert!(html.contains("prefers-reduced-motion"), "motion guard");
        assert!(html.contains("draggable"), "tab reorder");
        assert!(html.contains("id=\"disp\""), "domain display");
        assert!(html.contains("id=\"clr\""), "clear button");
        assert!(html.contains("id=\"sh\""), "shield in omnibox");
        assert!(html.contains("id=\"star\""), "bookmark star");
        assert!(html.contains("id=\"dl\""), "downloads button");
        // Tab-strip spec: tablist roles, chevron, middle-click, dblclick.
        assert!(html.contains("role=\"tablist\""), "aria tablist");
        assert!(html.contains("aria-selected"), "aria selected");
        assert!(html.contains("id=\"tablist\""), "overflow chevron");
        assert!(html.contains("auxclick"), "middle-click close");
        assert!(html.contains("toggle_maximize"), "dblclick maximize");
        // Toolbar spec: Rust-split display, overlay-hosted nav drops, combobox.
        assert!(!html.contains("new URL("), "no JS URL parsing");
        assert!(
            html.contains("navDrop('nav_back')") && html.contains("navDrop('nav_fwd')"),
            "nav drops post overlay popover_open with their rect"
        );
        assert!(html.contains("role=\"combobox\""), "aria combobox");
        assert!(html.contains("aria-activedescendant"), "active descendant");
        assert!(html.contains("role=\"toolbar\""), "aria toolbar");
        // Zoom chip: text percent; the popup itself (and its polite live
        // percent) is shell-overlay-hosted, so only chip + overlay wiring may
        // appear here.
        assert!(html.contains("id=\"zoom\""), "zoom chip");
        assert!(
            html.contains("cmd:'popover_open',kind:'zoom'"),
            "chip posts its rect"
        );
        assert!(
            !html.contains("zoompop"),
            "no chrome-DOM zoom popup beside the overlay host"
        );
        // The zoom step posts (zoom_in/out/reset) are gone from chrome:
        // rows act on the overlay's popover_activate, and the zoom arms
        // reachable from chrome are through that channel plus the menu ids.
        assert!(
            !html.contains("cmd:'zoom_in'")
                && !html.contains("cmd:'zoom_out'")
                && !html.contains("cmd:'zoom_reset'"),
            "chrome no longer posts zoom steps directly — the overlay rows do"
        );
        // Bookmarks bar: row, popovers, DnD, menubar pattern.
        assert!(html.contains("id=\"bookbar\""), "bar row");
        assert!(html.contains("role=\"menubar\""), "aria menubar");
        assert!(html.contains("bar_reorder"), "bar reorder");
        // 3c-6: the bookmarks-bar overflow root, folder drill-in, empty-bar
        // menu, star form, workspace switcher, profile switcher, group editor
        // and every context menu are shell-hosted overlay surfaces. Chrome
        // posts a typed `chrome_surface_open` (geometry + ids only) and the
        // shell resolves each target against live state.
        assert!(html.contains("chrome_surface_open"), "typed surface post");
        for surf in [
            "postSurface('bar_menu'",
            "postSurface('star'",
            "postSurface('workspaces'",
            "postSurface('profiles'",
            "postSurface('tab_context'",
            "postSurface('tabs_context'",
            "postSurface('group_context'",
            "postSurface('bookmark_context'",
        ] {
            assert!(html.contains(surf), "missing surface post: {surf}");
        }
        // The chrome-DOM popovers are deleted, not merely hidden (the shell
        // overlay is the only layer that paints above content).
        for gone in [
            "id=\"starpop\"",
            "id=\"barmenu\"",
            "id=\"barsub\"",
            "id=\"ctxpop\"",
            "id=\"gedit\"",
            "id=\"wspop\"",
            "id=\"profpop\"",
        ] {
            assert!(!html.contains(gone), "chrome-DOM popover left: {gone}");
        }
        for retired in ["hidePops", "showCtx", "openGedit", "__ferriteFolderList"] {
            assert!(
                !html.contains(retired),
                "chrome-DOM pop machinery left: {retired}"
            );
        }
        // ARCH-15: the overflow menu must survive the click that opened it.
        // The click-away is a mousedown that ignores the surface's own owner,
        // so a press already delivered for this gesture cannot retire it.
        assert!(
            html.contains("document.addEventListener('mousedown'"),
            "click-away is a mousedown"
        );
        assert!(
            html.contains("owner.contains(e.target)"),
            "click-away excludes the surface owner"
        );
        assert!(
            html.contains("surfOwner=el||null"),
            "opening press records its owner"
        );
        // Main menu rows live server-side now (shell `menu_static_rows`,
        // pinned by its own test): the chrome button only posts its rect.
        // What the template must still carry is the opener wiring.
        assert!(html.contains("menu_open"), "menu opener post");
        assert!(html.contains("getBoundingClientRect"), "menu anchor rect");
        // Omnibox dropdown: chrome keeps the same-document a11y listbox
        // (#sugs) and the action owner, and mirrors the visible list into the
        // overlay webview (chrome DOM is occluded below the content webview).
        // The relay hooks replay overlay clicks back here.
        assert!(html.contains("id=\"sugs\""), "a11y listbox mirror kept");
        assert!(html.contains("syncPopover"), "overlay mirror sync");
        assert!(html.contains("popover_open"), "popover open post");
        assert!(html.contains("popover_highlight"), "popover highlight post");
        assert!(html.contains("popover_close"), "popover close post");
        assert!(html.contains("popover_clipboard"), "popover clipboard post");
        // The clipboard row must outlive the blur the overlay click forces.
        assert!(html.contains("clipHeld"), "clipboard text survives blur");
        assert!(
            html.contains("__ferriteSugActivate"),
            "row activation relay"
        );
        assert!(html.contains("__ferriteSugDelete"), "row delete relay");
        // History/nav drops anchor under their button and open on touch
        // long-press as well as right-click (the long-press helper posts
        // the same overlay popover_open); reload disables with no tab.
        assert!(html.contains("holdToList"), "touch long-press");
        assert!(html.contains("relBtn.disabled=!at"), "reload disabled");
        // Status bar: hover preview zone (fed by the Rust init script's
        // link_hover IPC), shared shield entry, mono readouts.
        assert!(html.contains("hover_mine"), "hover gating");
        assert!(
            html.contains("kind:'shield'"),
            "shield entry posts the overlay pop"
        );
        // Shield honesty moved with the card: the session headline, the
        // per-site line and the pause command now live in the shell-hosted
        // overlay pop (`PopKind::Shield`, 3c-3) and are pinned by
        // ferrite-shell's `shield_pop_view_pins_rows_and_heights`. Chrome
        // keeps only the two entry points that post the owner rect.
        assert_eq!(
            html.matches("kind:'shield'").count(),
            2,
            "toolbar pill and status-strip count both open the overlay card"
        );
        assert!(!html.contains("shield_pause"), "no chrome pause post");
        assert!(html.contains("tabular-nums"), "mono readouts");
        // 3c-5: the desktop find card and toast are overlay windows now
        // (`ferrite://find` / `ferrite://toast`). The chrome document must
        // not keep a second copy that could paint under the page, and the
        // shell state they read no longer travels in ChromeState.
        assert!(!html.contains("id=\"toast\""), "desktop toast mount gone");
        assert!(!html.contains("renderToast"), "toast renderer gone");
        assert!(!html.contains("toast_action"), "no chrome toast post");
        assert!(
            !html.contains("S.status.toast"),
            "toast frame not in chrome"
        );
        assert!(!html.contains("id=\"findbar\""), "find card gone");
        assert!(!html.contains("id=\"findq\""), "find input gone");
        assert!(!html.contains("find_open"), "find state not in chrome");
        assert!(!html.contains("S.find_"), "find frame not in chrome");
        assert!(!html.contains("toastEl"), "toast DOM handles gone");
        // The load bar is fixed to the measured infobar bottom (which is the
        // content slot's top edge) instead of flowing under the page.
        assert!(html.contains("positionLoadbar"), "loadbar pinned in JS");
        assert!(
            html.contains("ib.getBoundingClientRect().bottom"),
            "loadbar measured from the band edge"
        );
        assert!(
            html.contains("if(e.propertyName==='max-height') positionLoadbar();"),
            "loadbar follows the infobar slide"
        );
        // Status strip: border-box so its 26px include the 1px top rule and
        // cannot drift from the shell's STATUS_H_PX content inset.
        assert!(
            html.contains("#status{box-sizing:border-box;"),
            "status border-box"
        );
        assert!(!html.contains("S.status.notice"), "notice pipeline retired");
        // Omnibox: tab-to-search provider mode (Tab commits a provider
        // row, Esc exits, Enter submits with the provider id).
        assert!(html.contains("tabsearchRow"), "provider row lookup");
        assert!(html.contains("enterProvider"), "provider commit");
        assert!(html.contains("exitProvider"), "provider exit");
        assert!(html.contains("provider:provId"), "provider submit");
        // Omnibox keyboard contract: highlighted provider row wins Tab,
        // Up reaches -1 (deselect), PgUp/PgDn jump, IME composition holds
        // Enter/input until commit, paste-and-go rides submitText+classify.
        assert!(html.contains("hl=(hl<=0)?-1:hl-1"), "up deselects");
        assert!(html.contains("PageDown"), "page jump down");
        assert!(html.contains("PageUp"), "page jump up");
        assert!(html.contains("isComposing"), "IME guard");
        assert!(html.contains("compositionend"), "IME commit flush");
        assert!(html.contains("paste-and-go"), "paste-and-go affordance");
        assert!(html.contains("clipboard.readText"), "clipboard read");
        // Strip icons name their tab (mute/pin carry ids; absent means
        // active server-side). Single-char queries render built rows.
        assert!(html.contains("toggle_mute',id:"), "mute carries id");
        assert!(html.contains("toggle_pin',id:"), "pin carries id");
        // Tooltip primitive: one shared popup, two tiers (simple + rich).
        assert!(html.contains("id=\"ferrite-tip\""), "tooltip mount");
        // Background-tab dialog badge (same quiet indicator language as
        // the audio/mute marks on the tab strip).
        assert!(html.contains("dialog_pending"), "dialog badge data");
        assert!(html.contains("is showing a dialog"), "dialog badge label");
        assert!(html.contains("role=\"tooltip\""), "tooltip role");
        assert!(html.contains("aria-describedby"), "tooltip labelling");
        assert!(html.contains("scanning?0:500"), "scan vs first-hover delay");
        assert!(html.contains("ftipArm(t,0)"), "focus trigger");
        assert!(html.contains("ftipHide"), "dismiss path");
        assert!(html.contains("ftipClean"), "bidi-safe tooltip text");
        assert!(html.contains("String.fromCharCode"), "no literal controls");
        // Rich preview state lines (from the pushed tab row, no new IPC).
        assert!(html.contains("Playing audio"), "audio state line");
        assert!(html.contains("Inactive: click to reload"), "discarded line");
        // Strip rebuilds swap every node: identical pushes must not churn
        // the DOM, and a changed strip retires tab-anchored tips.
        assert!(html.contains("tabsEl._lastH"), "rebuild compare");
        // Tooltip surfaces stay flat: hairline border, never a shadow.
        assert!(html.contains("#ferrite-tip{"), "tooltip style");
        assert!(html.contains("show rich"), "rich tab preview");
        assert!(html.contains("data-ftip-orig"), "native-title stash");
        // Icon kit: Phosphor Bold, inlined, currentColor, no glyphs/fetches.
        assert!(html.contains("viewBox=\"0 0 256 256\""), "phosphor grid");
        assert!(html.contains("fill=\"currentColor\""), "pack-tinted icons");
        assert!(html.contains("phosphoricons.com"), "attribution");
        // Weight language: Regular default, Bold lit, Fill toggled-on, with
        // a Regular fallback so unknown weights never blank an icon.
        assert!(html.contains("function icon(n,w)"), "weight-aware render");
        assert!(html.contains("icon('shield',n>0?'b':'r')"), "lit shield");
        assert!(
            html.contains("icon('star',S.starred?'f':'r')"),
            "saved star"
        );
        assert!(html.contains("icon('muted','f')"), "active mute");
        // Tactile layer: ghost completion mount, pointer drag with FLIP,
        // favicon faces, GPU-only motion, close reachability.
        assert!(html.contains("id=\"ghost\""), "ghost mount");
        assert!(html.contains("updateGhost"), "ghost renderer");
        assert!(html.contains("acceptGhost"), "ghost accept");
        assert!(html.contains("pointerdown"), "pointer drag");
        assert!(html.contains("setPointerCapture"), "drag capture");
        assert!(html.contains("requestAnimationFrame"), "FLIP frame");
        assert!(html.contains("stripRects"), "FLIP measure");
        assert!(
            html.contains("cubic-bezier(.2,.9,.25,1.15)"),
            "spring curve"
        );
        assert!(html.contains("class=\"fav\""), "favicon face");
        assert!(html.contains("class=\"ftav\""), "preview face");
        assert!(html.contains("translateX(-100%)"), "compositor loadbar");
        assert!(html.contains("submitText"), "envelope budget trim");
        assert!(html.contains("focus-within .x"), "close focus reach");
        assert!(html.contains("min-width:32px"), "touch targets");
        assert!(html.contains("pointer:coarse"), "coarse pointers");
        // Incognito entry: detective icon (regular-only, fallback-safe),
        // toolbar button doubling as the incognito indicator, menu row.
        assert!(html.contains("detective:{r:'<path"), "detective icon");
        assert!(html.contains("id=\"incogbtn\""), "incognito button");
        assert!(html.contains("S.incognito"), "incognito indicator state");
        assert!(html.contains("new-incognito"), "incognito menu row");
        // Session signal: pushed `t.private` renders a per-tab dot, and the
        // pushed `S.private_tab`/`S.session_kind` drive the toolbar tint.
        // Title-prefix text alone would not survive a template restyle.
        assert!(html.contains("t.private"), "private tab data read");
        assert!(html.contains("class=\"pdot\""), "private dot style");
        assert!(html.contains("S.private_tab"), "private tab state read");
        assert!(html.contains("S.session_kind"), "session kind read");
        assert!(html.contains("dataset.session"), "session dataset hook");
        assert!(html.contains("Private tab active"), "private toolbar label");
        // Static icons must init AFTER the kit: calling icon() while ICONS
        // is still undefined throws and kills the whole chrome script.
        let kit = html.find("var ICONS=").expect("kit defined");
        let init = html.find("backBtn.innerHTML").expect("static init");
        assert!(init > kit, "init after kit");
        for glyph in ["&#8592;", "&#8635;", "&#128737;", "&#9776;", "&#128274;"] {
            assert!(!html.contains(glyph), "unicode glyph left: {glyph}");
        }
        for banned in [
            "http://",
            "https://",
            "fetch(",
            "XMLHttpRequest",
            "eval(",
            "<iframe",
        ] {
            assert!(!html.contains(banned), "bundled only, found {banned}");
        }
    }

    #[test]
    fn mobile_template_shares_contract_and_stays_bundled() {
        // Same trust class as desktop: inline scripts only, no remote
        // fetches, typed commands over window.ipc, CSP locked.
        for hard in ["#131315", "#e5e1e4", "#1c1b1e"] {
            assert!(
                !CHROME_MOBILE_TEMPLATE.contains(hard),
                "hardcoded {hard} in mobile template"
            );
        }
        assert!(
            CHROME_MOBILE_TEMPLATE.contains("/*__FERRITE_THEME__*/"),
            "shared theme marker"
        );
        let html = chrome_mobile_html(ThemePack::VolcanicObsidian);
        assert!(!html.contains("__FERRITE_THEME__"), "marker replaced");
        assert!(html.contains("default-src 'none'"), "CSP locked");
        assert!(html.contains("__ferriteChrome"), "renderer hook");
        assert!(html.contains("__ferriteTheme"), "pack-switch hook");
        assert!(html.contains("request_state"), "bootstraps from shell push");
        assert!(html.contains("prefers-reduced-motion"), "motion guard");
        assert!(!html.contains("new URL("), "no JS URL parsing");
        for banned in [
            "http://",
            "https://",
            "fetch(",
            "XMLHttpRequest",
            "eval(",
            "<iframe",
        ] {
            assert!(!html.contains(banned), "bundled only, found {banned}");
        }
        // Mobile structure: bottom toolbar, switcher, omni sheet, menu
        // sheet; no desktop strip/popover ids leak in.
        assert!(html.contains("id=\"toolbar\""), "bottom toolbar");
        assert!(html.contains("role=\"toolbar\""), "aria toolbar");
        assert!(html.contains("id=\"switcher\""), "tab switcher");
        assert!(html.contains("id=\"swgrid\""), "switcher grid");
        assert!(html.contains("id=\"omnisheet\""), "omnibox sheet");
        assert!(html.contains("role=\"combobox\""), "aria combobox");
        assert!(html.contains("id=\"sheet\""), "menu sheet");
        assert!(html.contains("role=\"menu\""), "aria menu");
        assert!(html.contains("id=\"toast\""), "toast mount");
        assert!(html.contains("aria-live=\"polite\""), "live region");
        assert!(!html.contains("id=\"tabbar\""), "no desktop strip");
        assert!(!html.contains("id=\"bookbar\""), "no desktop bar");
        // Same command vocabulary as desktop (shell needs zero changes).
        for cmd in [
            "select_tab",
            "close_tab",
            "new_tab",
            "omnibox_input",
            "omnibox_submit",
            "focus_content",
            "focus_omnibox",
            "go_back",
            "go_forward",
            "reload",
            "share",
            "menu_item",
            "toggle_star",
            "toast_action",
            "toast_dismiss",
            "find_search",
            "find_next",
            "find_prev",
            "find_close",
            "shield_pause",
            "zoom_in",
            "zoom_out",
            "zoom_reset",
            "perm_decide",
            "translate_page",
            "dl_bubble_open",
            "dl_bubble_retry",
            "panel_history",
            "panel_section",
            "downloads_recent",
            "reading_list",
            "navigate",
            "crash_restore",
            "crash_dismiss",
        ] {
            assert!(html.contains(cmd), "mobile posts {cmd}");
        }
        // Same list hooks as desktop (shell pushes land somewhere).
        for hook in [
            "__ferritePanelHistory",
            "__ferritePanelBookmarks",
            "__ferriteDownloadsRecent",
            "__ferritePanelReading",
        ] {
            assert!(html.contains(hook), "mobile renders {hook}");
        }
        // Touch discipline: 44px+ targets, no hover-only affordances.
        assert!(html.contains("min-height:44px"), "touch targets");
        assert!(html.contains("min-height:48px"), "primary targets");
        assert!(html.contains("viewport-fit=cover"), "safe-area viewport");
        // Shield honesty mirrors desktop: counts read, pause posts the
        // hostless command (shell derives the host from the live tab).
        assert!(html.contains("shield_pause"), "pause command");
        assert!(html.contains("site_paused"), "pause state read");
    }

    #[test]
    fn every_pack_covers_compat_vars_and_geometry() {
        for pack in ThemePack::ALL {
            let css = pack.css();
            for var in [
                "--f-bg:",
                "--f-surface-1:",
                "--f-surface-2:",
                "--f-fg:",
                "--f-muted:",
                "--f-accent:",
                "--f-danger:",
                "--f-border:",
                "--f-font-ui:",
                "--pk-tab-radius:",
                "--pk-pill-radius:",
                "--pk-pop-radius:",
                "--pk-card-radius:",
                "--pk-ctl-radius:",
                "--pk-pop-blur:",
            ] {
                assert!(css.contains(var), "{var} missing in {}", pack.name());
            }
            assert!(
                css.contains(pack.accent()),
                "accent missing {}",
                pack.name()
            );
            // No remote fetches in any pack.
            for banned in ["http://", "https://", "url("] {
                assert!(!css.contains(banned), "{banned} in {}", pack.name());
            }
        }
    }

    #[test]
    fn pack_names_round_trip_and_cycle() {
        for pack in ThemePack::ALL {
            assert_eq!(ThemePack::from_name(pack.name()), Some(pack));
        }
        assert_eq!(ThemePack::from_name("nope"), None);
        assert_eq!(ThemePack::VolcanicObsidian.next(), ThemePack::Tenebra);
        assert_eq!(ThemePack::SolarSilk.next(), ThemePack::VolcanicObsidian);
    }

    #[test]
    fn frame_colors_are_valid_hex_with_correct_mode() {
        for pack in ThemePack::ALL {
            let bg = pack.frame_bg();
            assert_eq!(bg.len(), 7, "{}", pack.name());
            assert!(bg.starts_with('#'), "{}", pack.name());
            assert!(
                bg[1..].chars().all(|c| c.is_ascii_hexdigit()),
                "{}",
                pack.name()
            );
        }
        assert!(ThemePack::VolcanicObsidian.frame_dark());
        assert!(ThemePack::Tenebra.frame_dark());
        assert!(!ThemePack::Browserbase.frame_dark());
        assert!(!ThemePack::PrismVoid.frame_dark());
        assert!(!ThemePack::SolarSilk.frame_dark());
    }

    #[test]
    fn boot_accent_matches_default_pack() {
        // Cold-start state before the first shell push must already carry
        // the default pack's flare; otherwise the first paint flashes the
        // previous default. Keep in sync with the boot fallback pack.
        assert!(
            CHROME_TEMPLATE.contains("accent:'#ffc670'"),
            "boot accent is Tenebra amber"
        );
        assert!(
            !CHROME_TEMPLATE.contains("accent:'#ff8c00'"),
            "stale Volcanic boot accent"
        );
    }

    #[test]
    fn dropdown_dismiss_and_freshness_contract() {
        // Tab-scoped surfaces (star, context menus) belong to the shell now
        // (3c-6): it closes a card whose owning tab is no longer active, so
        // chrome keeps no per-tab pop identity of its own. History, downloads,
        // shield and permissions are overlay-hosted too (3c-2/3c-3/3c-4): the
        // shell re-derives their rows from live stores on every refresh (and
        // closes an emptied permission card), so no chrome-side fill-only guard
        // or click-away is needed and none of them is in a snapshot list.
        assert!(
            !CHROME_TEMPLATE.contains("lastSnapKey"),
            "no chrome-side tab-scoped snapshot tracking: the shell retires tab-owned surfaces"
        );
        // Click-away for every shell-hosted pop is a mousedown that ignores
        // the surface's own chrome owner, so the gesture that opened a
        // press-driven surface cannot retire it (ARCH-15).
        assert!(
            CHROME_TEMPLATE.contains("document.addEventListener('mousedown'"),
            "click-away is a mousedown, not a click"
        );
        assert!(
            CHROME_TEMPLATE.contains("owner.contains(e.target)"),
            "click-away excludes the open surface's chrome owner"
        );
        assert!(
            CHROME_TEMPLATE.contains("surfOwner=el||null"),
            "the opening press records the surface's owner"
        );
        // Downloads/shield are shell-hosted (3c-3) with no async list round
        // trip, so the loading placeholder and the fill-only re-open guard
        // die with the chrome-DOM bubble.
        assert!(
            !CHROME_TEMPLATE.contains("Loading…"),
            "no chrome-DOM async pop placeholder left"
        );
        assert!(
            !CHROME_TEMPLATE.contains("histpop")
                && !CHROME_TEMPLATE.contains("navpop")
                && !CHROME_TEMPLATE.contains("history_recent")
                && !CHROME_TEMPLATE.contains("hist_list")
                && !CHROME_TEMPLATE.contains("__ferriteBackList")
                && !CHROME_TEMPLATE.contains("__ferriteFwdList")
                && !CHROME_TEMPLATE.contains("__ferriteHistoryRecent")
                && !CHROME_TEMPLATE.contains("__ferriteClosedRecent")
                && !CHROME_TEMPLATE.contains("closed_reopen")
                && !CHROME_TEMPLATE.contains("closed_restore_all"),
            "no chrome-DOM history/nav pop copy and no dead commands left"
        );
    }

    #[test]
    fn permissions_pop_is_overlay_hosted_and_chrome_dom_removed() {
        // 3c-4: chrome keeps the chip (the affordance: icon, badge, attention
        // bump, aria-label, live region) and posts only its rect. The shell
        // owns the rows, the arming window and Not now, and closes the card
        // when the pending list empties. The chrome-DOM popover, its per-row
        // command vocabulary and its own arm timer are gone.
        assert!(
            CHROME_TEMPLATE.contains("post({cmd:'popover_open',kind:'permissions'"),
            "the chip opens the shell-hosted card"
        );
        for dead in [
            "#permpop",
            "renderPermPop",
            "permOpenedAt",
            "data-pallow",
            "data-palways",
            "permnotnow",
            "id=\"permwrap\"",
            "perm_decide",
            "pbtns",
        ] {
            assert!(
                !CHROME_TEMPLATE.contains(dead),
                "dead chrome-DOM permission surface left in desktop chrome: {dead}"
            );
        }
        // The chip's own a11y contract survives the migration: live region,
        // per-kind icon and the origin-bearing label.
        assert!(
            CHROME_TEMPLATE.contains("<span role=\"status\">"),
            "chip live region"
        );
        assert!(
            CHROME_TEMPLATE.contains("permission request"),
            "chip accessible name"
        );
        assert!(CHROME_TEMPLATE.contains("permIcon"), "chip icon map");
        assert!(
            CHROME_TEMPLATE.contains("#perm.attn{animation:bump"),
            "attention bump"
        );
        // The bundled mobile template has no overlay and still posts
        // `perm_decide` (pinned by
        // `mobile_template_shares_contract_and_stays_bundled`), so the IPC
        // variant is not dead: desktop reaches it through the pop's action
        // table instead of a chrome-DOM post.
        assert!(
            chrome_mobile_html(ThemePack::VolcanicObsidian).contains("perm_decide"),
            "mobile keeps the permission command"
        );
    }

    /// Icon-table keys are `    name:{` lines inside the `ICONS` literal.
    /// Shared by the pin-collision and cross-template parity tests below.
    #[cfg(test)]
    fn icon_keys(html: &str) -> Vec<&str> {
        html.lines()
            .filter_map(|l| {
                if !l.starts_with("    ") || l[4..].starts_with(' ') {
                    return None;
                }
                let (name, _) = l.trim_start().split_once(":{")?;
                if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase()) {
                    return None;
                }
                Some(name)
            })
            .collect()
    }

    #[test]
    fn pin_key_has_no_collision() {
        // The map-pin and push-pin shared one key; the second definition
        // silently won and geolocation prompts showed a push-pin.
        let keys = icon_keys(CHROME_TEMPLATE);
        assert_eq!(
            keys.iter().filter(|k| **k == "pin").count(),
            1,
            "exactly one push-pin"
        );
        assert_eq!(
            keys.iter().filter(|k| **k == "pinmap").count(),
            1,
            "exactly one map-pin"
        );
        assert!(
            CHROME_TEMPLATE.contains("geolocation:'pinmap'"),
            "permission icon uses the map-pin"
        );
    }

    #[test]
    fn mobile_icon_kit_matches_desktop() {
        // Same keys, same weights: the mobile shell draws from the same
        // vocabulary, so a desktop-tested icon never blanks on mobile.
        let mut desktop = icon_keys(CHROME_TEMPLATE);
        let mut mobile = icon_keys(CHROME_MOBILE_TEMPLATE);
        desktop.sort_unstable();
        mobile.sort_unstable();
        assert_eq!(mobile, desktop, "kit parity across templates");
        for (btn, name) in [
            ("back", "back"),
            ("fwd", "fwd"),
            ("rel", "reload"),
            ("tabcount", "caret"),
            ("sharebtn", "share"),
            ("menu", "menu"),
        ] {
            assert!(
                CHROME_MOBILE_TEMPLATE.contains(&format!("setIc('{btn}','{name}')")),
                "mobile toolbar wires {btn} to {name}"
            );
        }
        for used in [
            "icon('lock')",
            "icon('info')",
            "icon('shield')",
            "icon('star',",
            "icon('caret')",
            "icon('x')",
        ] {
            assert!(
                CHROME_MOBILE_TEMPLATE.contains(used),
                "mobile renders {used}"
            );
        }
    }

    // ---- Bar-row visibility contract ------------------------------------
    //
    // The bar row's two layouts (blurred crumb plus page actions, editing
    // omnibox) have one owner, `paintBarRow`. Every control whose visibility
    // the script owns is hidden by the inline `display:none` in the markup,
    // never by a stylesheet rule: the script shows such a control by
    // *clearing* that inline declaration, and a cleared inline falls back to
    // the rule, so a rule there makes the control unreachable. That is what
    // had happened to the bookmark star, reader, translate, the bookmarks-bar
    // overflow chevron and the history filter row. The tests below derive
    // both halves from the served asset, so re-introducing a rule, a second
    // focus site or a second visibility owner fails here.

    /// CSS `(selector, declarations)` pairs. At-rule preludes (`@media`,
    /// `@keyframes`) are skipped, but their bodies are scanned: a
    /// `display:none` inside a media query is just as unreachable.
    fn css_rules(css: &str) -> Vec<(String, String)> {
        let mut rules = Vec::new();
        let mut open: Vec<String> = Vec::new();
        let mut buf = String::new();
        for ch in css.chars() {
            match ch {
                '{' => open.push(std::mem::take(&mut buf)),
                '}' => {
                    let decls = std::mem::take(&mut buf);
                    if let Some(sel) = open.pop()
                        && !sel.trim_start().starts_with('@')
                    {
                        rules.push((sel, decls));
                    }
                }
                _ => buf.push(ch),
            }
        }
        rules
    }

    /// Every `<style>` body in the document, concatenated.
    fn stylesheets(html: &str) -> String {
        let mut out = String::new();
        let mut rest = html;
        while let Some(open) = rest.find("<style") {
            let after = &rest[open..];
            let Some(head) = after.find('>') else { break };
            let body = &rest[open + head + 1..];
            let Some(end) = body.find("</style>") else {
                break;
            };
            out.push_str(&body[..end]);
            out.push('\n');
            rest = &body[end + "</style>".len()..];
        }
        out
    }

    /// The variable name an assignment target ends with: `X=` and `X =`
    /// both resolve to `X`, and a `.prop` write resolves to the variable it
    /// was taken from. Empty when the text ends inside a call.
    fn name_before(text: &str) -> String {
        let head = text.trim_end();
        let head = head.strip_suffix('=').unwrap_or(head);
        head.chars()
            .rev()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    /// The id an inline `document.getElementById('id')` in `text` ends with.
    fn inline_id_before(text: &str) -> Option<String> {
        let head = text.trim_end();
        let call = head.rfind("document.getElementById('")?;
        let after = &head[call + "document.getElementById('".len()..];
        let quote = after.find('\'')?;
        Some(after[..quote].to_string())
    }

    /// `variable -> element id`, from `name=document.getElementById('id')`.
    fn id_bindings(html: &str) -> Vec<(String, String)> {
        let needle = "document.getElementById('";
        let mut binds = Vec::new();
        let mut rest = html;
        while let Some(at) = rest.find(needle) {
            let after = &rest[at + needle.len()..];
            if let Some(quote) = after.find('\'') {
                let name = name_before(&rest[..at]);
                if !name.is_empty() {
                    binds.push((name, after[..quote].to_string()));
                }
            }
            rest = after;
        }
        binds
    }

    /// Ids the script shows by clearing the inline display, i.e. those with
    /// a `X.style.display` write whose value contains `''`, resolved through
    /// [`id_bindings`] or an inline `getElementById` call.
    fn cleared_ids(html: &str) -> Vec<String> {
        let binds = id_bindings(html);
        let mut ids: Vec<String> = Vec::new();
        let mut rest = html;
        while let Some(at) = rest.find(".style.display") {
            let after = &rest[at + ".style.display".len()..];
            // An assignment, not a read: `X.style.display!==''` is a probe.
            let rhs = after.trim_start();
            let Some(rhs) = rhs.strip_prefix('=').filter(|r| !r.starts_with('=')) else {
                rest = after;
                continue;
            };
            let rhs_end = rhs.find([';', '\n']).unwrap_or(rhs.len());
            if rhs[..rhs_end].contains("''") {
                // Only the statement holding the write, so an unrelated
                // getElementById earlier in the file cannot be picked up.
                let head = &rest[..at];
                let stmt_end = head.trim_end().strip_suffix('=').unwrap_or(head.trim_end());
                let stmt = stmt_end
                    .rsplit([';', '{', '}', '\n'])
                    .next()
                    .unwrap_or(stmt_end);
                let name = name_before(stmt);
                let id = binds
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, id)| id.clone())
                    .or_else(|| inline_id_before(stmt));
                if let Some(id) = id
                    && !ids.contains(&id)
                {
                    ids.push(id);
                }
            }
            rest = after;
        }
        ids
    }

    /// Whether the subject compound of `selector` is exactly `#id` (so
    /// `#star` never matches the `#starpop` rule).
    fn selector_is_id(selector: &str, id: &str) -> bool {
        let subject = selector
            .rsplit([' ', '>', '+', '~'])
            .next()
            .unwrap_or(selector);
        let needle = format!("#{id}");
        let mut rest = subject;
        while let Some(at) = rest.find(&needle) {
            let tail = &rest[at + needle.len()..];
            let next = tail.chars().next();
            if !matches!(next, Some(c) if c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return true;
            }
            rest = tail;
        }
        false
    }

    /// CSS with `/* ... */` comments removed, so a comment can never be read
    /// as part of a selector or of a declaration block.
    fn strip_comments(css: &str) -> String {
        let mut out = String::with_capacity(css.len());
        let mut rest = css;
        while let Some(open) = rest.find("/*") {
            out.push_str(&rest[..open]);
            let Some(close) = rest[open + 2..].find("*/") else {
                return out; // unterminated: drop the tail
            };
            rest = &rest[open + 2 + close + 2..];
        }
        out.push_str(rest);
        out
    }

    /// The selector hiding `#id` with `display:none`, if there is one.
    fn hidden_by_rule(css: &str, id: &str) -> Option<String> {
        for (selector, decls) in css_rules(&strip_comments(css)) {
            let flat: String = decls.chars().filter(|c| !c.is_whitespace()).collect();
            if flat.contains("display:none") && selector.split(',').any(|s| selector_is_id(s, id)) {
                return Some(selector.trim().to_string());
            }
        }
        None
    }

    /// The opening tag carrying `id="id"`.
    fn markup_tag<'a>(html: &'a str, id: &str) -> Option<&'a str> {
        let needle = format!("id=\"{id}\"");
        let mut from = 0;
        while let Some(at) = html[from..].find(&needle) {
            let pos = from + at;
            if let (Some(start), Some(end)) = (html[..pos].rfind('<'), html[pos..].find('>')) {
                return Some(&html[start..=pos + end]);
            }
            from = pos + needle.len();
        }
        None
    }

    /// The body of the first `function name(){...}` in the asset.
    fn function_body<'a>(html: &'a str, name: &str) -> &'a str {
        let head = format!("function {name}(){{");
        let open = html
            .find(&head)
            .unwrap_or_else(|| panic!("{name} missing from template"));
        let start = open + head.len();
        let close = start
            + html[start..]
                .find("\n  }")
                .unwrap_or_else(|| panic!("{name} body never closes"));
        &html[start..close]
    }

    #[test]
    fn detector_catches_the_stylesheet_hidden_clear_pattern() {
        // The contract test above only shows that the shipped asset is clean,
        // so the detector is pinned here on both sides: the trap must be
        // reported, the inline-hidden form must pass, a sibling id must not
        // implicate its prefix, and reading the property is not a write.
        let binding = "var star=document.getElementById('star');";
        let buggy = format!(
            "<style>#star{{color:red;display:none;}}</style>\
             <button id=\"star\"></button><script>{binding} star.style.display=show?'':'none';</script>"
        );
        assert_eq!(cleared_ids(&buggy), vec!["star".to_string()]);
        assert_eq!(
            hidden_by_rule(&stylesheets(&buggy), "star").as_deref(),
            Some("#star"),
            "a rule that hides a clear-to-show control must be reported"
        );
        let fixed = format!(
            "<style>#star{{color:red;}}</style>\
             <button id=\"star\" style=\"display:none\"></button>\
             <script>{binding} star.style.display=show?'':'none';</script>"
        );
        assert_eq!(cleared_ids(&fixed), vec!["star".to_string()]);
        assert_eq!(hidden_by_rule(&stylesheets(&fixed), "star"), None);
        let sibling = format!("<style>#starpop{{display:none;}}</style><script>{binding}</script>");
        assert_eq!(
            hidden_by_rule(&stylesheets(&sibling), "star"),
            None,
            "#starpop is not #star"
        );
        assert!(
            cleared_ids(
                "<script>var open=v.style.display!=='none'&&v.style.display!=='';</script>"
            )
            .is_empty(),
            "reading style.display is not a clear-to-show write"
        );
    }

    #[test]
    fn clear_to_show_controls_are_never_stylesheet_hidden() {
        let ids = cleared_ids(CHROME_TEMPLATE);
        // Completeness guard: a parse that stops finding controls would make
        // the assertion below vacuous.
        for must in [
            "omni",
            "clr",
            "star",
            "readbtn",
            "transbtn",
            "sharebtn",
            "pipbtn",
            "scopechip",
            "perm",
            "zoom",
            "psearch",
            "padd",
            "barover",
        ] {
            assert!(
                ids.iter().any(|id| id == must),
                "#{must} is no longer derived as a clear-to-show control"
            );
        }
        let css = stylesheets(CHROME_TEMPLATE);
        assert!(
            css.contains("display:none"),
            "stylesheet parse found no display declarations"
        );
        for id in ids {
            if let Some(rule) = hidden_by_rule(&css, &id) {
                panic!(
                    "#{id} is shown by clearing its inline display, so `{rule}` leaves it \
                     permanently hidden; hide it with the markup's inline display:none instead"
                );
            }
        }
    }

    #[test]
    fn controls_hidden_at_first_paint_carry_inline_display_none() {
        // The script's only hidden value is `none` and its only show gesture
        // is clearing the inline declaration, so a control that must not be
        // on screen before the first render has to declare that state in the
        // served markup. #omni is deliberately absent: the bar starts
        // visible and the first push hides it.
        for id in [
            "clr", "star", "readbtn", "transbtn", "sharebtn", "pipbtn", "barover", "psearch",
        ] {
            let tag = markup_tag(CHROME_TEMPLATE, id)
                .unwrap_or_else(|| panic!("no markup tag with id={id}"));
            assert!(
                tag.contains("display:none"),
                "#{id} must start hidden: {tag}"
            );
        }
    }

    #[test]
    fn omnibox_focus_has_a_single_entry_point() {
        // The bar is display:none whenever it is blurred, and a hidden input
        // cannot take the caret, so the un-hide and the focus have to stay
        // together behind one name.
        assert_eq!(
            CHROME_TEMPLATE.matches("omni.focus()").count(),
            1,
            "omnibox focus belongs to focusOmnibox alone"
        );
        assert_eq!(
            CHROME_TEMPLATE.matches("omni.select()").count(),
            1,
            "select-on-entry belongs to selectOmnibox alone"
        );
        assert!(
            CHROME_TEMPLATE.contains("window.__ferriteFocusOmnibox=selectOmnibox;"),
            "the shell drives the entry point through __ferriteFocusOmnibox"
        );
        let body = function_body(CHROME_TEMPLATE, "focusOmnibox");
        let unhide = body
            .find("omni.style.display=''")
            .expect("focusOmnibox un-hides the bar");
        let focus = body
            .find("omni.focus()")
            .expect("focusOmnibox focuses the bar");
        assert!(
            unhide < focus,
            "un-hide must come first or the focus is lost"
        );
        assert!(
            body.contains("paintBarRow();"),
            "focus moves with no push behind it, so the row is repainted here"
        );
    }

    #[test]
    fn bar_row_visibility_has_exactly_one_owner() {
        for id in ["star", "readbtn", "transbtn", "sharebtn", "pipbtn", "clr"] {
            assert_eq!(
                CHROME_TEMPLATE
                    .matches(&format!("{id}.style.display"))
                    .count(),
                1,
                "{id} visibility must have a single write site"
            );
        }
        let row = function_body(CHROME_TEMPLATE, "paintBarRow");
        for control in ["star", "readbtn", "transbtn", "sharebtn", "pipbtn"] {
            assert!(
                row.contains(&format!("{control}.style.display")),
                "{control} visibility belongs to paintBarRow"
            );
        }
        assert!(
            function_body(CHROME_TEMPLATE, "syncClr").contains("clr.style.display"),
            "the clear-× rule belongs to syncClr"
        );
        assert!(
            CHROME_TEMPLATE.matches("paintBarRow();").count() >= 2,
            "render and focusOmnibox both repaint the row"
        );
    }

    #[test]
    fn mobile_template_toggles_visibility_by_class_not_inline_display() {
        // The mobile template cannot carry the desktop trap: it never writes
        // `style.display` for visibility (it toggles classes). If that ever
        // changes, the desktop contract above has to be extended to cover it
        // rather than letting the clear-to-show pattern grow unguarded.
        assert!(
            !CHROME_MOBILE_TEMPLATE.contains(".style.display"),
            "mobile toggles visibility by class; an inline-display write needs the desktop net"
        );
    }

    #[test]
    fn zoom_pop_is_overlay_hosted_and_chrome_dom_removed() {
        // The zoom popup moved to the shell overlay (D1 of
        // tasks/toolbar-overlay-3c.md): chrome posts only the chip rect; the
        // DOM copy must be deleted, never re-added beside a live overlay.
        assert!(
            !CHROME_TEMPLATE.contains("zoompop"),
            "#zoompop DOM/CSS beyond removal — the overlay owns this popup now"
        );
        assert!(
            !CHROME_TEMPLATE.contains("renderZoomPop"),
            "renderZoomPop is gone with its DOM copy"
        );
        assert!(
            !CHROME_TEMPLATE.contains("data-zoom"),
            "zoompop row actions are gone with the DOM copy"
        );
        // The chip posts the anchor rect; the shell toggles, sizes, and
        // refreshes — chrome carries no toggle logic for this kind.
        assert!(
            CHROME_TEMPLATE.contains("cmd:'popover_open',kind:'zoom'"),
            "the chip posts a zoom popover_open with its rect"
        );
        assert!(
            CHROME_TEMPLATE.contains("window.__ferritePopState"),
            "aria-expanded rides the shell push (D3)"
        );
        assert!(
            CHROME_TEMPLATE.contains("cmd:'popover_close',kind:'*'"),
            "chrome click-away/Esc dismiss the open shell pop"
        );
        assert!(
            !CHROME_TEMPLATE.contains("zoompop.style.display"),
            "chrome never shows/hides the migrated pop itself"
        );
    }

    #[test]
    fn nav_and_history_pops_are_overlay_hosted_and_chrome_dom_removed() {
        // Slices 3c-2 of tasks/toolbar-overlay-3c.md: the back/forward drops
        // and the history dropdown moved to the shell overlay. Chrome posts
        // only anchor rects; the DOM copies, their CSS, and the hook feeds
        // must be deleted, never re-added beside a live overlay.
        assert!(
            !CHROME_TEMPLATE.contains("navpop") && !CHROME_TEMPLATE.contains("histpop"),
            "no nav/history DOM, CSS, or hide-list entries may remain"
        );
        assert!(
            !CHROME_TEMPLATE.contains("histItems") && !CHROME_TEMPLATE.contains("closedItems"),
            "the off-screen cache arrays die with the DOM render"
        );
        assert!(
            !CHROME_TEMPLATE.contains("data-steps")
                && !CHROME_TEMPLATE.contains("data-cidx")
                && !CHROME_TEMPLATE.contains("data-hurl")
                && !CHROME_TEMPLATE.contains("data-call=\"1\"")
                && !CHROME_TEMPLATE.contains("data-hall"),
            "DOM row action attributes die with the DOM render"
        );
        assert!(
            !CHROME_TEMPLATE.contains("cmd:'hist_list'")
                && !CHROME_TEMPLATE.contains("cmd:'history_recent'")
                && !CHROME_TEMPLATE.contains("cmd:'closed_reopen'")
                && !CHROME_TEMPLATE.contains("cmd:'closed_restore_all'"),
            "the old hook commands are gone from chrome"
        );
        assert!(
            !CHROME_TEMPLATE.contains("__ferriteBackList")
                && !CHROME_TEMPLATE.contains("__ferriteFwdList")
                && !CHROME_TEMPLATE.contains("__ferriteHistoryRecent")
                && !CHROME_TEMPLATE.contains("__ferriteClosedRecent"),
            "the hook feed functions die with the DOM render"
        );
        // The three owners post their rects by kind; the shell owns toggling.
        assert!(
            CHROME_TEMPLATE.contains("navDrop('nav_back')")
                && CHROME_TEMPLATE.contains("navDrop('nav_fwd')")
                && CHROME_TEMPLATE.contains("kind:'history'"),
            "back, forward, and history buttons post overlay popover_open"
        );
        // aria-expanded fans out per kind's owner button (D3 generalization).
        for owner in [
            "zoom:'zoom'",
            "nav_back:'back'",
            "nav_fwd:'fwd'",
            "history:'hist'",
            "downloads:'dl'",
            "shield:'sh'",
        ] {
            assert!(
                CHROME_TEMPLATE.contains(owner),
                "the owner map is the single aria-expanded driver: {owner}"
            );
        }
        assert!(
            !CHROME_TEMPLATE.contains("var popOpen=null"),
            "the toolbar pop kind keeps its own variable, apart from the omnibox mirror's popOpen"
        );
        assert!(
            CHROME_TEMPLATE.contains("var toolPop=null"),
            "toolbar pop kind state is toolPop"
        );
    }

    #[test]
    fn downloads_and_shield_pops_are_overlay_hosted_and_chrome_dom_removed() {
        // Task 3c-3: the desktop downloads bubble and shield card moved to
        // the shell-hosted overlay. No chrome DOM copy, no data-feed hooks,
        // no per-row command vocabulary may survive: chrome posts the owner
        // rect and the shell renders rows from its own stores.
        for dead in [
            "dlpop",
            "shieldpop",
            "openShield",
            "openDl",
            "fmtDlSize",
            "__ferriteDownloadsRecent",
            "downloads_recent",
            "dl_bubble_open",
            "dl_bubble_folder",
            "dl_bubble_retry",
            "dl_bubble_remove",
            "data-dlopen",
            "data-dlfolder",
            "data-dlretry",
            "data-dlremove",
            "data-dlall",
            "data-manage",
            "data-cert",
            "data-site",
            "#pausebtn",
        ] {
            assert!(
                !CHROME_TEMPLATE.contains(dead),
                "chrome-DOM downloads/shield copy must be gone: {dead}"
            );
        }
        // Both owners post their rect; the shell owns open/toggle/content.
        assert!(
            CHROME_TEMPLATE.contains("kind:'downloads'"),
            "downloads button posts the overlay pop"
        );
        assert_eq!(
            CHROME_TEMPLATE.matches("kind:'shield'").count(),
            2,
            "toolbar pill and status-strip count both post the shield pop"
        );
        // They ride the same aria-expanded owner map as the other kinds.
        assert!(CHROME_TEMPLATE.contains("downloads:'dl'"));
        assert!(CHROME_TEMPLATE.contains("shield:'sh'"));
        // The unwired mobile bundle keeps its own bubble vocabulary: this
        // migration is the desktop asset only.
        assert!(CHROME_MOBILE_TEMPLATE.contains("dl_bubble_open"));
    }
}
