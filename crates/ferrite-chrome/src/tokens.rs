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
        // Toolbar spec: Rust-split display, nav dropdowns, combobox.
        assert!(!html.contains("new URL("), "no JS URL parsing");
        assert!(html.contains("__ferriteBackList"), "back dropdown");
        assert!(html.contains("role=\"combobox\""), "aria combobox");
        assert!(html.contains("aria-activedescendant"), "active descendant");
        assert!(html.contains("role=\"toolbar\""), "aria toolbar");
        // Zoom chip + popup: text percent, named step buttons, polite live.
        assert!(html.contains("id=\"zoom\""), "zoom chip");
        assert!(html.contains("id=\"zoompop\""), "zoom popup");
        assert!(html.contains("zoom_in"), "zoom in post");
        assert!(html.contains("aria-live=\"polite\""), "zoom announce");
        // Bookmarks bar: row, popovers, DnD, menubar pattern.
        assert!(html.contains("id=\"bookbar\""), "bar row");
        assert!(html.contains("role=\"menubar\""), "aria menubar");
        assert!(html.contains("__ferriteFolderList"), "folder dropdown");
        assert!(html.contains("bookmark_all_tabs"), "bookmark-all");
        assert!(html.contains("bar_reorder"), "bar reorder");
        // Main menu rows live server-side now (shell `menu_static_rows`,
        // pinned by its own test): the chrome button only posts its rect.
        // What the template must still carry is the opener wiring.
        assert!(html.contains("menu_open"), "menu opener post");
        assert!(html.contains("getBoundingClientRect"), "menu anchor rect");
        // History dropdowns anchor under their button and open on touch
        // long-press as well as right-click; reload disables with no tab.
        assert!(html.contains("r.left"), "navpop anchor");
        assert!(html.contains("holdToList"), "touch long-press");
        assert!(html.contains("relBtn.disabled=!at"), "reload disabled");
        // Status bar: hover preview zone (fed by the Rust init script's
        // link_hover IPC), shared shield entry, mono readouts.
        assert!(html.contains("hover_mine"), "hover gating");
        assert!(html.contains("openShield"), "shared shield entry");
        // Shield honesty: session headline labeled as such, per-site row
        // from pushed state, pause toggle posting a hostless command (the
        // shell derives the host from the live tab).
        assert!(html.contains("blocked this session"), "session headline");
        assert!(html.contains("blocked on this site"), "per-site row");
        assert!(html.contains("shield_pause"), "pause command");
        assert!(html.contains("site_paused"), "pause state read");
        assert!(html.contains("site_blocked"), "per-site count read");
        assert!(html.contains("tabular-nums"), "mono readouts");
        // Toast primitive: queue-head renderer (shell owns the queue).
        assert!(html.contains("id=\"toast\""), "toast mount");
        assert!(html.contains("aria-live=\"polite\""), "toast live region");
        assert!(html.contains("renderToast"), "toast renderer");
        assert!(html.contains("toast_action"), "toast action post");
        assert!(html.contains("toast_dismiss"), "toast dismiss post");
        assert!(html.contains("toastSeq"), "toast frame tracking");
        assert!(html.contains("pauseToastTimer"), "hover/focus pause");
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
        // Snapshot pops (per-tab data, rendered once on open) hide on tab
        // or URL change; live pops (menu/ws/prof) re-render instead.
        assert!(
            CHROME_TEMPLATE.contains("lastSnapKey"),
            "tab-change identity tracking"
        );
        assert!(
            CHROME_TEMPLATE.contains(
                "['shieldpop','histpop','navpop','zoompop','starpop','permpop','ctxpop']"
            ),
            "snapshot hide list"
        );
        // Click-away covers every toggle pop, zoom and permissions included.
        assert!(
            CHROME_TEMPLATE.contains("#zoompop')&&!e.target.closest('#zoom')"),
            "zoom click-away"
        );
        assert!(
            CHROME_TEMPLATE.contains("#permpop')&&!e.target.closest('#perm')"),
            "permission click-away"
        );
        // Async pops never dead-click and never pop back open after close.
        assert!(CHROME_TEMPLATE.contains("Loading…"), "loading placeholder");
        assert!(CHROME_TEMPLATE.contains("wasOpen"), "dl fill-only guard");
        assert!(
            CHROME_TEMPLATE.contains("histWasOpen"),
            "history fill-only guard"
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
}
