//! Embedded fonts: JetBrains Mono and Geist Pixel Square.

use egui::{Context, FontData, FontDefinitions, FontFamily};

pub const JETBRAINS_MONO_TTF: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");
pub const GEIST_PIXEL_SQUARE_TTF: &[u8] = include_bytes!("../assets/fonts/GeistPixel-Square.ttf");

pub fn geist_pixel_family() -> FontFamily {
    FontFamily::Name("geist_pixel".into())
}

pub fn definitions() -> FontDefinitions {
    let mut def = FontDefinitions::default();

    def.font_data.insert(
        "jetbrains_mono".to_owned(),
        std::sync::Arc::new(FontData::from_owned(JETBRAINS_MONO_TTF.to_vec())),
    );
    def.font_data.insert(
        "geist_pixel_square".to_owned(),
        std::sync::Arc::new(FontData::from_owned(GEIST_PIXEL_SQUARE_TTF.to_vec())),
    );

    if let Some(family) = def.families.get_mut(&FontFamily::Monospace) {
        family.insert(0, "jetbrains_mono".to_owned());
    }

    def.families.insert(
        geist_pixel_family(),
        vec!["geist_pixel_square".to_owned(), "jetbrains_mono".to_owned()],
    );

    if let Some(family) = def.families.get_mut(&FontFamily::Monospace) {
        family.push("geist_pixel_square".to_owned());
    }

    def
}

pub fn install(ctx: &Context) {
    ctx.set_fonts(definitions());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_fonts_are_non_trivial() {
        assert!(JETBRAINS_MONO_TTF.len() > 100_000);
        assert!(GEIST_PIXEL_SQUARE_TTF.len() > 10_000);
        assert_eq!(&JETBRAINS_MONO_TTF[0..4], b"\x00\x01\x00\x00");
        assert_eq!(&GEIST_PIXEL_SQUARE_TTF[0..4], b"\x00\x01\x00\x00");
    }

    #[test]
    fn definitions_register_families() {
        let defs = definitions();
        assert!(defs.font_data.contains_key("jetbrains_mono"));
        assert!(defs.font_data.contains_key("geist_pixel_square"));
        let mono = &defs.families[&FontFamily::Monospace];
        assert_eq!(mono.first().map(String::as_str), Some("jetbrains_mono"));
        assert!(mono.contains(&"geist_pixel_square".to_owned()));
        assert!(defs.families.contains_key(&geist_pixel_family()));
    }
}
