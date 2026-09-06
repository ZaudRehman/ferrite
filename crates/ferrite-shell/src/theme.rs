//! Theme tokens: colors, corners, and accent context.
//!
//! Palette blends Aetherial Mercury surfaces with the Technical UI Kit
//! (Geist Pixel + JetBrains Mono + Lucide). Structural and interactive
//! elements both use sharp corners (rounded-sm 2px max). See [`crate::fonts`].

use egui::{
    Color32, Context, CornerRadius, FontId, Shadow, Stroke, Visuals,
    style::{Selection, WidgetVisuals},
};

// ── Surface palette (Technical UI Kit) ──
pub const SUBSTRATE: Color32 = Color32::from_rgb(0x09, 0x0A, 0x0F);
pub const SURFACE_1: Color32 = Color32::from_rgb(0x12, 0x15, 0x1E);
pub const SURFACE_2: Color32 = Color32::from_rgb(0x1A, 0x1E, 0x2B);
pub const SURFACE_3: Color32 = Color32::from_rgb(0x24, 0x29, 0x38);

// ── Typography palette ──
pub const FOREGROUND: Color32 = Color32::from_rgb(0xF8, 0xFA, 0xFC);
#[allow(dead_code)]
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0x94, 0xA3, 0xB8);
#[allow(dead_code)]
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x64, 0x74, 0x8B);

// ── Accent palette ──
pub const ACCENT_PRIMARY: Color32 = Color32::from_rgb(0x00, 0xF2, 0xC3);
pub const ACCENT_INTERACTIVE: Color32 = Color32::from_rgb(0x4F, 0x46, 0xE5);
pub const ACCENT_DANGER: Color32 = Color32::from_rgb(0xFF, 0x47, 0x57);
pub const ACCENT_ARCHIVE: Color32 = Color32::from_rgb(0xF0, 0xC7, 0x5E);
pub const ACCENT_WARNING: Color32 = Color32::from_rgb(0xFF, 0xB7, 0x4A);

// ── Border tokens ──
#[allow(dead_code)]
pub const BORDER_SUBTLE: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 20);
pub const BORDER_ACTIVE: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 64);
pub const HAIRLINE: Color32 = BORDER_SUBTLE;

// ── Shape language: sharp 0px (structural), rounded-sm 2px (interactive), pill 16px omnibox ──
pub const CORNER_SHARP: CornerRadius = CornerRadius::same(0);
pub const CORNER_SM: CornerRadius = CornerRadius::same(2);
pub const CORNER_OMNIBOX: CornerRadius = CornerRadius::same(16);
#[allow(dead_code)]
pub const CORNER_PILL: CornerRadius = CornerRadius::same(u8::MAX);

/// Kit blueprint — Lucide + Geist Pixel + JetBrains Mono technical kit
/// (borders over shadows, rounded-sm 2px, tabular-nums)
#[allow(dead_code)]
pub mod kit {
    use super::Color32;
    pub const BG_APP: Color32 = Color32::from_rgb(0x09, 0x0A, 0x0F);
    pub const BG_SURFACE: Color32 = Color32::from_rgb(0x12, 0x15, 0x1E);
    pub const BG_ELEVATED: Color32 = Color32::from_rgb(0x1A, 0x1E, 0x2B);
    pub const BORDER_SUBTLE: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 20);
    pub const BORDER_ACTIVE: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 64);
    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xF8, 0xFA, 0xFC);
    pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0x94, 0xA3, 0xB8);
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x64, 0x74, 0x8B);
    pub const ACCENT_PRIMARY: Color32 = Color32::from_rgb(0x00, 0xF2, 0xC3);
    pub const ACCENT_ALERT: Color32 = Color32::from_rgb(0xFF, 0x47, 0x57);
    /// Lucide icon style: 1.75px stroke, 20px box, outline-only, round join
    pub const ICON_STROKE: f32 = 1.75;
    pub const ICON_BOX: f32 = 20.0;
}

/// Spacing scale 4/8/12/16/24/32/48/64 per Refactoring UI density
pub mod spacing {
    #[allow(dead_code)]
    pub const S1: f32 = 4.0;
    #[allow(dead_code)]
    pub const S2: f32 = 8.0;
    #[allow(dead_code)]
    pub const S3: f32 = 12.0;
    #[allow(dead_code)]
    pub const S4: f32 = 16.0;
    #[allow(dead_code)]
    pub const S6: f32 = 24.0;
    #[allow(dead_code)]
    pub const S8: f32 = 32.0;
    #[allow(dead_code)]
    pub const S12: f32 = 48.0;
    #[allow(dead_code)]
    pub const S16: f32 = 64.0;
}
/// Motion tokens per Refactoring UI
pub mod motion {
    #[allow(dead_code)]
    pub const FAST_MS: f32 = 100.0;
    #[allow(dead_code)]
    pub const NORMAL_MS: f32 = 150.0;
    #[allow(dead_code)]
    pub const TOAST_MS: f32 = 200.0;
}
/// Inputs use sharp corners with a bottom-only border.
#[allow(dead_code)]
pub const CORNER_INPUT: CornerRadius = CORNER_SHARP;
pub const INPUT_BORDER_WIDTH: f32 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteTokens {
    pub substrate: Color32,
    pub surface_1: Color32,
    pub surface_2: Color32,
    pub surface_3: Color32,
    pub foreground: Color32,
    pub accent_primary: Color32,
    pub accent_interactive: Color32,
    pub accent_danger: Color32,
    pub accent_archive: Color32,
    pub accent_warning: Color32,
}

impl Default for PaletteTokens {
    #[inline]
    fn default() -> Self {
        Self::aetherial_mercury()
    }
}

impl PaletteTokens {
    /// The canonical palette — Aetherial Mercury surfaces with Technical UI Kit accents.
    pub const fn aetherial_mercury() -> Self {
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

impl AccentContext {
    pub fn toggle(self) -> Self {
        match self {
            Self::Standard => Self::Archive,
            Self::Archive => Self::Standard,
        }
    }

    pub fn debug_label(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Archive => "archive",
        }
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
            palette: PaletteTokens::aetherial_mercury(),
            accent_context: AccentContext::Standard,
        }
    }
}

impl ThemeConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Active accent for the current context.
    pub fn resolved_interactive_accent(&self) -> Color32 {
        match self.accent_context {
            AccentContext::Standard => self.palette.accent_interactive,
            AccentContext::Archive => self.palette.accent_archive,
        }
    }

    pub fn toggle_context(&mut self) -> AccentContext {
        self.accent_context = self.accent_context.toggle();
        self.accent_context
    }

    pub fn visuals(&self) -> Visuals {
        let p = &self.palette;
        let iris = self.resolved_interactive_accent();
        let selection_fill = iris.gamma_multiply(0.35);
        let _active_fill = iris.gamma_multiply(0.55);

        let noninteractive = WidgetVisuals {
            bg_fill: p.surface_1,
            weak_bg_fill: p.surface_1.gamma_multiply(0.6),
            bg_stroke: Stroke::new(1.0, HAIRLINE),
            fg_stroke: Stroke::new(1.0, p.foreground),
            corner_radius: CORNER_SHARP,
            expansion: 0.0,
        };

        // Interactive states: rounded-sm 2px — sleek: ghost idle, subtle hover
        let inactive = WidgetVisuals {
            bg_fill: egui::Color32::TRANSPARENT,
            weak_bg_fill: p.surface_1,
            bg_stroke: Stroke::NONE,
            fg_stroke: Stroke::new(1.0, p.foreground.gamma_multiply(0.85)),
            corner_radius: CORNER_SM,
            expansion: 0.0,
        };
        let hovered = WidgetVisuals {
            bg_fill: p.surface_2,
            weak_bg_fill: p.surface_1,
            bg_stroke: Stroke::new(1.0, BORDER_SUBTLE),
            fg_stroke: Stroke::new(1.0, p.foreground),
            corner_radius: CORNER_SM,
            expansion: 0.0,
        };
        let active = WidgetVisuals {
            bg_fill: p.surface_2,
            weak_bg_fill: p.surface_2,
            bg_stroke: Stroke::new(1.0, iris.gamma_multiply(0.6)),
            fg_stroke: Stroke::new(1.0, p.foreground),
            corner_radius: CORNER_SM,
            expansion: 0.0,
        };
        let open = WidgetVisuals {
            bg_stroke: Stroke::new(1.0, BORDER_ACTIVE),
            ..inactive
        };

        let mut visuals = Visuals::dark();
        visuals.dark_mode = true;
        visuals.override_text_color = Some(p.foreground);
        visuals.widgets.noninteractive = noninteractive;
        visuals.widgets.inactive = inactive;
        visuals.widgets.open = open;
        visuals.widgets.hovered = hovered;
        visuals.widgets.active = active;

        // All corners sharp — borders over shadows
        visuals.window_corner_radius = CORNER_SHARP;
        visuals.menu_corner_radius = CORNER_SM;
        visuals.panel_fill = p.surface_1;
        visuals.window_fill = p.surface_2;
        visuals.extreme_bg_color = p.substrate;
        visuals.faint_bg_color = p.surface_1;
        visuals.text_edit_bg_color = Some(p.substrate);
        visuals.code_bg_color = p.surface_2;
        visuals.selection = Selection {
            bg_fill: selection_fill,
            stroke: Stroke::new(1.0, iris),
        };
        visuals.hyperlink_color = iris;
        visuals.warn_fg_color = p.accent_warning;
        visuals.error_fg_color = p.accent_danger;
        visuals.window_stroke = Stroke::new(1.0, HAIRLINE);
        visuals.window_highlight_topmost = false;
        visuals.popup_shadow = Shadow::NONE;
        visuals.window_shadow = Shadow::NONE;
        visuals
    }

    pub fn apply(&self, ctx: &Context) {
        ctx.set_visuals(self.visuals());
    }

    /// Bottom border for inputs. Focused uses the active accent.
    pub fn input_bottom_stroke(&self, focused: bool) -> Stroke {
        let color = if focused {
            self.resolved_interactive_accent()
        } else {
            HAIRLINE
        };
        Stroke::new(INPUT_BORDER_WIDTH, color)
    }

    pub fn to_hex(c: Color32) -> String {
        format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
    }

    pub fn telemetry_font(size: f32) -> FontId {
        FontId::monospace(size)
    }

    pub fn technical_label_font(size: f32) -> FontId {
        FontId::new(size, crate::fonts::geist_pixel_family())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_hex_values_are_brand_compliant() {
        assert_eq!(SUBSTRATE, Color32::from_rgb(0x09, 0x0A, 0x0F));
        assert_eq!(SURFACE_1, Color32::from_rgb(0x12, 0x15, 0x1E));
        assert_eq!(SURFACE_2, Color32::from_rgb(0x1A, 0x1E, 0x2B));
        assert_eq!(SURFACE_3, Color32::from_rgb(0x24, 0x29, 0x38));
        assert_eq!(FOREGROUND, Color32::from_rgb(0xF8, 0xFA, 0xFC));
        assert_eq!(ACCENT_PRIMARY, Color32::from_rgb(0x00, 0xF2, 0xC3));
        assert_eq!(ACCENT_INTERACTIVE, Color32::from_rgb(0x4F, 0x46, 0xE5));
        assert_eq!(ACCENT_DANGER, Color32::from_rgb(0xFF, 0x47, 0x57));
        assert_eq!(ACCENT_ARCHIVE, Color32::from_rgb(0xF0, 0xC7, 0x5E));
        assert_eq!(ACCENT_WARNING, Color32::from_rgb(0xFF, 0xB7, 0x4A));
    }

    #[test]
    fn input_field_third_category_border_states() {
        let mut cfg = ThemeConfig::default();
        assert_eq!(CORNER_INPUT, CORNER_SHARP);
        assert_eq!(cfg.input_bottom_stroke(false).width, INPUT_BORDER_WIDTH);

        // Resting = border_subtle; focused Standard = Ghostly Iris.
        assert_eq!(cfg.input_bottom_stroke(false).color, HAIRLINE);
        assert_eq!(cfg.input_bottom_stroke(true).color, ACCENT_INTERACTIVE);

        // Focused while Archive context = Pulsar Gold.
        cfg.accent_context = AccentContext::Archive;
        assert_eq!(cfg.input_bottom_stroke(true).color, ACCENT_ARCHIVE);
    }

    #[test]
    fn to_hex_formats_uppercase() {
        assert_eq!(ThemeConfig::to_hex(ACCENT_INTERACTIVE), "#4F46E5");
        assert_eq!(ThemeConfig::to_hex(ACCENT_ARCHIVE), "#F0C75E");
        assert_eq!(ThemeConfig::to_hex(SUBSTRATE), "#090A0F");
    }

    #[test]
    fn aetherial_mercury_is_default() {
        assert_eq!(PaletteTokens::default(), PaletteTokens::aetherial_mercury());
        assert_eq!(AccentContext::default(), AccentContext::Standard);
        assert_eq!(
            ThemeConfig::default().palette,
            PaletteTokens::aetherial_mercury()
        );
    }

    #[test]
    fn shape_language_sharp_and_sm() {
        assert_eq!(CORNER_SHARP, CornerRadius::same(0));
        assert_eq!(CORNER_SM, CornerRadius::same(2));
        let v = ThemeConfig::default().visuals();
        assert_eq!(v.widgets.noninteractive.corner_radius, CORNER_SHARP);
        assert_eq!(v.widgets.inactive.corner_radius, CORNER_SM);
        assert_eq!(v.widgets.hovered.corner_radius, CORNER_SM);
        assert_eq!(v.widgets.active.corner_radius, CORNER_SM);
        assert_eq!(v.window_corner_radius, CORNER_SHARP);
        assert_eq!(v.menu_corner_radius, CORNER_SM);
    }

    #[test]
    fn validation_6_resolved_accent_switches_and_danger_constant() {
        let mut cfg = ThemeConfig::default();

        // Standard → Ghostly Iris
        assert_eq!(cfg.resolved_interactive_accent(), ACCENT_INTERACTIVE);
        assert_eq!(cfg.visuals().hyperlink_color, ACCENT_INTERACTIVE);

        // Toggle → Archive → Pulsar Gold, danger and warning untouched
        let flipped = cfg.toggle_context();
        assert_eq!(flipped, AccentContext::Archive);
        assert_eq!(cfg.resolved_interactive_accent(), ACCENT_ARCHIVE);
        assert_eq!(cfg.visuals().hyperlink_color, ACCENT_ARCHIVE);
        assert_eq!(cfg.visuals().error_fg_color, ACCENT_DANGER);
        assert_eq!(cfg.visuals().warn_fg_color, ACCENT_WARNING);
        assert_eq!(cfg.palette.accent_primary, ACCENT_PRIMARY);

        // Toggle back within one call chain
        assert_eq!(cfg.toggle_context(), AccentContext::Standard);
        assert_eq!(cfg.resolved_interactive_accent(), ACCENT_INTERACTIVE);
    }

    #[test]
    fn accent_toggle_round_trip() {
        assert_eq!(AccentContext::Standard.toggle(), AccentContext::Archive);
        assert_eq!(AccentContext::Archive.toggle(), AccentContext::Standard);
    }

    #[test]
    fn surfaces_layer_monotonically() {
        let v = ThemeConfig::default().visuals();
        assert_eq!(v.panel_fill, SURFACE_1);
        assert_eq!(v.window_fill, SURFACE_2);
        assert_eq!(v.extreme_bg_color, SUBSTRATE);
    }

    #[test]
    fn fonts_helpers_target_right_families() {
        assert_eq!(
            ThemeConfig::telemetry_font(14.0).family,
            egui::FontFamily::Monospace
        );
        assert_eq!(
            ThemeConfig::technical_label_font(11.0).family,
            crate::fonts::geist_pixel_family()
        );
    }
}
