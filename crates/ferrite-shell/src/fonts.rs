//! Embedded fonts served to privileged pages via `ferrite://assets/`.
//!
//! No toolkit registration: the chrome webview loads these through the
//! `@font-face` block in [`crate::theme::ThemeConfig::theme_css`].

pub const JETBRAINS_MONO_TTF: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");
pub const GEIST_PIXEL_SQUARE_TTF: &[u8] = include_bytes!("../assets/fonts/GeistPixel-Square.ttf");

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
}
