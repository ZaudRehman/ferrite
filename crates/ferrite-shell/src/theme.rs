//! Theme tokens: colors, corners, and accent context.
//!
//! Palette is Aetherial Mercury. Structural surfaces use sharp corners;
//! interactive elements use pill corners. See [`crate::fonts`] for type.

use egui::{
    Color32, Context, CornerRadius, FontId, Shadow, Stroke, Visuals,
    style::{Selection, WidgetVisuals},
};

pub const SUBSTRATE: Color32 = Color32::from_rgb(0x02, 0x02, 0x03);
pub const SURFACE_1: Color32 = Color32::from_rgb(0x13, 0x13, 0x13);
pub const SURFACE_2: Color32 = Color32::from_rgb(0x1C, 0x1B, 0x1B);
pub const SURFACE_3: Color32 = Color32::from_rgb(0x2A, 0x2A, 0x2A);
pub const FOREGROUND: Color32 = Color32::from_rgb(0xE5, 0xE2, 0xE1);
pub const ACCENT_PRIMARY: Color32 = Color32::from_rgb(0xE5, 0xE5, 0xE5);
pub const ACCENT_INTERACTIVE: Color32 = Color32::from_rgb(0x4F, 0x46, 0xE5);
pub const ACCENT_DANGER: Color32 = Color32::from_rgb(0xFF, 0x00, 0x3F);
pub const ACCENT_ARCHIVE: Color32 = Color32::from_rgb(0xF0, 0xC7, 0x5E);

pub const HAIRLINE: Color32 = SURFACE_3;

pub const CORNER_SHARP: CornerRadius = CornerRadius::same(0);
pub const CORNER_PILL: CornerRadius = CornerRadius::same(u8::MAX);
/// Inputs use sharp corners with a bottom-only border.
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
}

impl Default for PaletteTokens {
    #[inline]
    fn default() -> Self {
        Self::aetherial_mercury()
    }
}

impl PaletteTokens {
    /// The canonical Aetherial Mercury palette.
    ///
    /// Hex values are encoded directly as RGB bytes (const-evaluable, no
    /// `unwrap` per CODING_STANDARDS §2.2).
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
        let active_fill = iris.gamma_multiply(0.55);

        let noninteractive = WidgetVisuals {
            bg_fill: p.surface_1,
            weak_bg_fill: p.surface_1.gamma_multiply(0.6),
            bg_stroke: Stroke::new(1.0, HAIRLINE),
            fg_stroke: Stroke::new(1.0, p.foreground),
            corner_radius: CORNER_SHARP,
            expansion: 0.0,
        };

        // Interactive states share the pill shape.
        let inactive = WidgetVisuals {
            bg_fill: p.surface_2,
            weak_bg_fill: p.surface_1,
            bg_stroke: Stroke::new(1.0, HAIRLINE),
            fg_stroke: Stroke::new(1.0, p.foreground),
            corner_radius: CORNER_PILL,
            expansion: 0.0,
        };
        let hovered = WidgetVisuals {
            bg_fill: p.surface_3,
            weak_bg_fill: p.surface_2,
            bg_stroke: Stroke::new(1.0, iris),
            fg_stroke: Stroke::new(1.0, p.foreground),
            corner_radius: CORNER_PILL,
            expansion: 0.0,
        };
        let active = WidgetVisuals {
            bg_fill: active_fill,
            weak_bg_fill: active_fill,
            bg_stroke: Stroke::new(1.0, ACCENT_PRIMARY),
            fg_stroke: Stroke::new(1.0, p.substrate),
            corner_radius: CORNER_PILL,
            expansion: 0.0,
        };
        let open = WidgetVisuals {
            bg_stroke: Stroke::new(1.0, ACCENT_PRIMARY),
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

        // Structural surfaces are sharp; windows/popovers use surface levels.
        visuals.window_corner_radius = CORNER_SHARP;
        visuals.menu_corner_radius = CORNER_PILL;
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
        visuals.warn_fg_color = p.accent_danger;
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
        assert_eq!(SUBSTRATE, Color32::from_rgb(0x02, 0x02, 0x03));
        assert_eq!(SURFACE_1, Color32::from_rgb(0x13, 0x13, 0x13));
        assert_eq!(SURFACE_2, Color32::from_rgb(0x1C, 0x1B, 0x1B));
        assert_eq!(SURFACE_3, Color32::from_rgb(0x2A, 0x2A, 0x2A));
        assert_eq!(FOREGROUND, Color32::from_rgb(0xE5, 0xE2, 0xE1));
        assert_eq!(ACCENT_PRIMARY, Color32::from_rgb(0xE5, 0xE5, 0xE5));
        assert_eq!(ACCENT_INTERACTIVE, Color32::from_rgb(0x4F, 0x46, 0xE5));
        assert_eq!(ACCENT_DANGER, Color32::from_rgb(0xFF, 0x00, 0x3F));
        assert_eq!(ACCENT_ARCHIVE, Color32::from_rgb(0xF0, 0xC7, 0x5E));
    }

    #[test]
    fn input_field_third_category_border_states() {
        let mut cfg = ThemeConfig::default();
        // Geometry is sharp; width fixed hairline.
        assert_eq!(CORNER_INPUT, CORNER_SHARP);
        assert_eq!(cfg.input_bottom_stroke(false).width, INPUT_BORDER_WIDTH);

        // Resting = hairline; focused Standard = Ghostly Iris.
        assert_eq!(cfg.input_bottom_stroke(false).color, HAIRLINE);
        assert_eq!(cfg.input_bottom_stroke(true).color, ACCENT_INTERACTIVE);

        // Focused while Archive context = Pulsar Gold (validation #7).
        cfg.accent_context = AccentContext::Archive;
        assert_eq!(cfg.input_bottom_stroke(true).color, ACCENT_ARCHIVE);
    }

    #[test]
    fn to_hex_formats_uppercase() {
        assert_eq!(ThemeConfig::to_hex(ACCENT_INTERACTIVE), "#4F46E5");
        assert_eq!(ThemeConfig::to_hex(ACCENT_ARCHIVE), "#F0C75E");
        assert_eq!(ThemeConfig::to_hex(SUBSTRATE), "#020203");
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
    fn shape_language_sharp_vs_pill() {
        assert_eq!(CORNER_SHARP, CornerRadius::same(0));
        assert_eq!(CORNER_PILL.nw, u8::MAX);
        let v = ThemeConfig::default().visuals();
        assert_eq!(v.widgets.noninteractive.corner_radius, CORNER_SHARP);
        assert_eq!(v.widgets.inactive.corner_radius, CORNER_PILL);
        assert_eq!(v.widgets.hovered.corner_radius, CORNER_PILL);
        assert_eq!(v.widgets.active.corner_radius, CORNER_PILL);
        assert_eq!(v.window_corner_radius, CORNER_SHARP);
    }

    #[test]
    fn validation_6_resolved_accent_switches_and_danger_constant() {
        let mut cfg = ThemeConfig::default();

        // Standard → Ghostly Iris
        assert_eq!(cfg.resolved_interactive_accent(), ACCENT_INTERACTIVE);
        assert_eq!(cfg.visuals().hyperlink_color, ACCENT_INTERACTIVE);

        // Toggle → Archive → Pulsar Gold, danger untouched
        let flipped = cfg.toggle_context();
        assert_eq!(flipped, AccentContext::Archive);
        assert_eq!(cfg.resolved_interactive_accent(), ACCENT_ARCHIVE);
        assert_eq!(cfg.visuals().hyperlink_color, ACCENT_ARCHIVE);
        assert_eq!(cfg.visuals().error_fg_color, ACCENT_DANGER);
        assert_eq!(cfg.visuals().warn_fg_color, ACCENT_DANGER);
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
