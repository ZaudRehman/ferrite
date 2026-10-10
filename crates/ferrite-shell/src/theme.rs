//! Theme tokens served as CSS. Owns the `/*__FERRITE_THEME__*/` block for
//! `ferrite://` pages. Chrome inlines its pack on top by cascade.

// ── Surface palette (hex, lowercase to match the served CSS contract) ──
// Tenebra-aligned base layer: warm umber blacks so the pre-pack flash and
// any pack-missing fallback stay in the brand family, never teal/iris.
pub const SUBSTRATE: &str = "#0d0a06";
pub const SURFACE_1: &str = "#120e08";
pub const SURFACE_2: &str = "#171209";
pub const SURFACE_3: &str = "#201a0f";

// ── Typography palette ──
pub const FOREGROUND: &str = "#f0e7d5";
#[allow(dead_code)]
pub const TEXT_SECONDARY: &str = "#cdbfa4";
#[allow(dead_code)]
pub const TEXT_MUTED: &str = "#8f7c57";

// ── Accent palette ──
pub const ACCENT_PRIMARY: &str = "#f4b762";
pub const ACCENT_INTERACTIVE: &str = "#ffc670";
pub const ACCENT_DANGER: &str = "#ffb4ab";
pub const ACCENT_ARCHIVE: &str = "#f0c75e";
pub const ACCENT_WARNING: &str = "#f4b762";

// ── Border tokens ──
#[allow(dead_code)]
pub const BORDER_SUBTLE: &str = "rgba(255,255,255,0.078)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteTokens {
    pub substrate: &'static str,
    pub surface_1: &'static str,
    pub surface_2: &'static str,
    pub surface_3: &'static str,
    pub foreground: &'static str,
    pub accent_primary: &'static str,
    pub accent_interactive: &'static str,
    pub accent_danger: &'static str,
    pub accent_archive: &'static str,
    pub accent_warning: &'static str,
}

impl Default for PaletteTokens {
    #[inline]
    fn default() -> Self {
        Self::tenebra()
    }
}

impl PaletteTokens {
    pub const fn tenebra() -> Self {
        Self {
            substrate: SUBSTRATE,
            surface_1: SURFACE_1,
            surface_2: SURFACE_2,
            surface_3: SURFACE_3,
            foreground: FOREGROUND,
            accent_primary: ACCENT_PRIMARY,
            accent_interactive: ACCENT_INTERACTIVE,
            accent_danger: ACCENT_DANGER,
            accent_archive: ACCENT_ARCHIVE,
            accent_warning: ACCENT_WARNING,
        }
    }

    /// Legacy cool-gray base (pre-Tenebra). Kept for compat; not the default.
    pub const fn aetherial_mercury() -> Self {
        Self {
            substrate: "#090a0f",
            surface_1: "#12151e",
            surface_2: "#1a1e2b",
            surface_3: "#242938",
            foreground: "#f8fafc",
            accent_primary: "#00f2c3",
            accent_interactive: "#4f46e5",
            accent_danger: "#ff4757",
            accent_archive: "#f0c75e",
            accent_warning: "#ffb74a",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccentContext {
    Standard,
    Archive,
}

impl Default for AccentContext {
    #[inline]
    fn default() -> Self {
        Self::Standard
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeConfig {
    pub palette: PaletteTokens,
    pub accent_context: AccentContext,
}

impl Default for ThemeConfig {
    #[inline]
    fn default() -> Self {
        Self {
            palette: PaletteTokens::tenebra(),
            accent_context: AccentContext::Standard,
        }
    }
}

impl ThemeConfig {
    /// Active accent for the current context, as a CSS hex string.
    pub fn resolved_interactive_accent(&self) -> &'static str {
        match self.accent_context {
            AccentContext::Standard => self.palette.accent_interactive,
            AccentContext::Archive => self.palette.accent_archive,
        }
    }

    /// CSS custom-property block for `ferrite://` pages. Includes `@font-face`
    /// for the embedded fonts (see `WryFactory::with_asset`).
    pub fn theme_css(&self) -> String {
        let p = &self.palette;
        format!(
            ":root{{--f-bg:{};--f-surface-1:{};--f-surface-2:{};--f-fg:{};--f-muted:{};--f-faint:{};--f-accent:{};--f-accent-primary:{};--f-danger:{};--f-warning:{};--f-archive:{};--f-border:{};--f-font-ui:{};--f-font-pixel:{};--f-font-mono:{}}}@font-face{{font-family:\"Geist Pixel Square\";src:url(\"http://ferrite.assets/geist-pixel-square.ttf\") format(\"truetype\");font-display:swap}}@font-face{{font-family:\"JetBrains Mono\";src:url(\"http://ferrite.assets/jetbrains-mono-regular.ttf\") format(\"truetype\");font-display:swap}}",
            p.substrate,
            p.surface_1,
            p.surface_2,
            p.foreground,
            TEXT_SECONDARY,
            TEXT_MUTED,
            self.resolved_interactive_accent(),
            p.accent_primary,
            p.accent_danger,
            p.accent_warning,
            p.accent_archive,
            BORDER_SUBTLE,
            "system-ui,-apple-system,\"Segoe UI\",sans-serif",
            "\"Geist Pixel Square\",ui-monospace,monospace",
            "\"JetBrains Mono\",ui-monospace,SFMono-Regular,monospace",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_hex_values_are_brand_compliant() {
        assert_eq!(SUBSTRATE, "#0d0a06");
        assert_eq!(SURFACE_1, "#120e08");
        assert_eq!(SURFACE_2, "#171209");
        assert_eq!(SURFACE_3, "#201a0f");
        assert_eq!(FOREGROUND, "#f0e7d5");
        assert_eq!(ACCENT_PRIMARY, "#f4b762");
        assert_eq!(ACCENT_INTERACTIVE, "#ffc670");
        assert_eq!(ACCENT_DANGER, "#ffb4ab");
        assert_eq!(ACCENT_ARCHIVE, "#f0c75e");
        assert_eq!(ACCENT_WARNING, "#f4b762");
    }

    #[test]
    fn tenebra_is_default() {
        assert_eq!(PaletteTokens::default(), PaletteTokens::tenebra());
        assert_eq!(AccentContext::default(), AccentContext::Standard);
        assert_eq!(ThemeConfig::default().palette, PaletteTokens::tenebra());
    }

    #[test]
    fn resolved_accent_switches_with_context() {
        let cfg = ThemeConfig::default();
        assert_eq!(cfg.resolved_interactive_accent(), ACCENT_INTERACTIVE);
        let cfg = ThemeConfig {
            accent_context: AccentContext::Archive,
            ..ThemeConfig::default()
        };
        assert_eq!(cfg.resolved_interactive_accent(), ACCENT_ARCHIVE);
    }

    #[test]
    fn theme_css_exports_tokens_and_fonts() {
        let css = ThemeConfig::default().theme_css();
        // Same contract the old Visuals-backed builder served.
        assert!(css.contains("--f-bg:#0d0a06"), "substrate {css}");
        assert!(css.contains("--f-fg:#f0e7d5"), "foreground {css}");
        assert!(css.contains("--f-accent:#ffc670"), "amber {css}");
        assert!(css.contains("--f-danger:#ffb4ab"), "danger {css}");
        assert!(
            css.contains("--f-border:rgba(255,255,255,0.078)"),
            "border {css}"
        );
        assert!(css.contains("--f-font-ui:"), "ui stack {css}");
        // Embedded fonts served from the asset table.
        assert!(css.contains("@font-face"), "faces {css}");
        assert!(css.contains("geist-pixel-square.ttf"), "pixel {css}");
        assert!(css.contains("jetbrains-mono-regular.ttf"), "mono {css}");
        // Archive accent context moves the interactive accent.
        let cfg = ThemeConfig {
            accent_context: AccentContext::Archive,
            ..ThemeConfig::default()
        };
        assert!(
            cfg.theme_css().contains("--f-accent:#f0c75e"),
            "archive accent"
        );
    }
}
