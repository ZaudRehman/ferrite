//! Custom titlebar per 1.1 Windowing Base — optional egui drawn when OS decorations disabled.

use egui::{RichText, Ui};
use winit::window::Window;

use crate::theme::SURFACE_1;

#[allow(dead_code)]
pub fn show_titlebar(ui: &mut Ui, window: &Window, title: &str) {
    egui::Frame::default()
        .fill(SURFACE_1)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("◧").size(14.0));
                ui.label(RichText::new(title).strong().size(13.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_sized(egui::vec2(32.0, 24.0), egui::Button::new("×"))
                        .on_hover_text("Close")
                        .clicked()
                    {
                        // winit close requested via window
                        let _ = title;
                    }
                    if ui
                        .add_sized(egui::vec2(32.0, 24.0), egui::Button::new("□"))
                        .on_hover_text("Maximize")
                        .clicked()
                    {
                        window.set_maximized(!window.is_maximized());
                    }
                    if ui
                        .add_sized(egui::vec2(32.0, 24.0), egui::Button::new("—"))
                        .on_hover_text("Minimize")
                        .clicked()
                    {
                        window.set_minimized(true);
                    }
                });
            });
        });
}
