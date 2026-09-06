//! Lucide icon strokes for Ferrite shell.
//!
//! Monoline, 1.75px, 20px box, outline-only, round joins — per Technical UI Kit.
//! Grid is Lucide 24×24, scaled to a 20×20 centered box.

use egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2};

pub const STROKE: f32 = 1.75;
pub const BOX: f32 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    ArrowLeft,
    ArrowRight,
    RotateCw,
    X,
    House,
    Shield,
    ShieldCheck,
    Bookmark,
    Terminal,
    Menu,
    Clock,
    Search,
    Plus,
    Audio,
    AudioMuted,
    Pin,
    PinFilled,
    Lock,
    Star,
    StarFilled,
    Info,
}

fn p(rect: Rect, x: f32, y: f32) -> Pos2 {
    let c = rect.center();
    let s = BOX / 24.0;
    c + Vec2::new((x - 12.0) * s, (y - 12.0) * s)
}

fn seg(painter: &Painter, a: Pos2, b: Pos2, color: Color32) {
    painter.line_segment([a, b], Stroke::new(STROKE, color));
}

fn polyline(painter: &Painter, pts: &[Pos2], color: Color32) {
    if pts.len() < 2 {
        return;
    }
    for w in pts.windows(2) {
        seg(painter, w[0], w[1], color);
    }
}

/// Draw `icon` centered in `rect` (assumed ~20×20 or larger) with `color`.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let stroke = Stroke::new(STROKE, color);
    match icon {
        Icon::ArrowLeft => {
            seg(painter, p(rect, 19.0, 12.0), p(rect, 5.0, 12.0), color);
            polyline(
                painter,
                &[p(rect, 12.0, 19.0), p(rect, 5.0, 12.0), p(rect, 12.0, 5.0)],
                color,
            );
        }
        Icon::ArrowRight => {
            seg(painter, p(rect, 5.0, 12.0), p(rect, 19.0, 12.0), color);
            polyline(
                painter,
                &[p(rect, 12.0, 5.0), p(rect, 19.0, 12.0), p(rect, 12.0, 19.0)],
                color,
            );
        }
        Icon::RotateCw => {
            // circle r=7 + top-right arrow
            painter.circle_stroke(p(rect, 12.0, 12.0), 7.0 * BOX / 24.0, stroke);
            // arrow head at ~20,8
            polyline(
                painter,
                &[p(rect, 16.0, 8.0), p(rect, 20.0, 8.0), p(rect, 20.0, 12.0)],
                color,
            );
        }
        Icon::X => {
            seg(painter, p(rect, 6.0, 6.0), p(rect, 18.0, 18.0), color);
            seg(painter, p(rect, 18.0, 6.0), p(rect, 6.0, 18.0), color);
        }
        Icon::House => {
            // roof + walls + doorway
            polyline(
                painter,
                &[
                    p(rect, 3.0, 9.5),
                    p(rect, 12.0, 3.0),
                    p(rect, 21.0, 9.5),
                    p(rect, 21.0, 21.0),
                    p(rect, 3.0, 21.0),
                    p(rect, 3.0, 9.5),
                ],
                color,
            );
            polyline(
                painter,
                &[
                    p(rect, 9.0, 21.0),
                    p(rect, 9.0, 13.0),
                    p(rect, 15.0, 13.0),
                    p(rect, 15.0, 21.0),
                ],
                color,
            );
        }
        Icon::Shield => {
            polyline(
                painter,
                &[
                    p(rect, 12.0, 22.0),
                    p(rect, 20.0, 18.0),
                    p(rect, 20.0, 8.5),
                    p(rect, 12.0, 3.5),
                    p(rect, 4.0, 8.5),
                    p(rect, 4.0, 18.0),
                    p(rect, 12.0, 22.0),
                ],
                color,
            );
        }
        Icon::ShieldCheck => {
            polyline(
                painter,
                &[
                    p(rect, 12.0, 22.0),
                    p(rect, 20.0, 18.0),
                    p(rect, 20.0, 8.5),
                    p(rect, 12.0, 3.5),
                    p(rect, 4.0, 8.5),
                    p(rect, 4.0, 18.0),
                    p(rect, 12.0, 22.0),
                ],
                color,
            );
            polyline(
                painter,
                &[p(rect, 9.0, 12.0), p(rect, 11.0, 14.0), p(rect, 15.0, 9.0)],
                color,
            );
        }
        Icon::Bookmark => {
            polyline(
                painter,
                &[
                    p(rect, 5.0, 21.0),
                    p(rect, 5.0, 3.0),
                    p(rect, 19.0, 3.0),
                    p(rect, 19.0, 21.0),
                    p(rect, 12.0, 17.0),
                    p(rect, 5.0, 21.0),
                ],
                color,
            );
        }
        Icon::Terminal => {
            polyline(
                painter,
                &[p(rect, 4.0, 17.0), p(rect, 10.0, 11.0), p(rect, 4.0, 5.0)],
                color,
            );
            seg(painter, p(rect, 12.0, 19.0), p(rect, 20.0, 19.0), color);
        }
        Icon::Menu => {
            seg(painter, p(rect, 4.0, 6.0), p(rect, 20.0, 6.0), color);
            seg(painter, p(rect, 4.0, 12.0), p(rect, 20.0, 12.0), color);
            seg(painter, p(rect, 4.0, 18.0), p(rect, 20.0, 18.0), color);
        }
        Icon::Clock => {
            painter.circle_stroke(p(rect, 12.0, 12.0), 8.0 * BOX / 24.0, stroke);
            polyline(painter, &[p(rect, 12.0, 12.0), p(rect, 12.0, 7.0)], color);
            seg(painter, p(rect, 12.0, 12.0), p(rect, 16.0, 14.0), color);
        }
        Icon::Search => {
            painter.circle_stroke(p(rect, 11.0, 11.0), 6.0 * BOX / 24.0, stroke);
            seg(painter, p(rect, 16.5, 16.5), p(rect, 20.0, 20.0), color);
        }
        Icon::Plus => {
            seg(painter, p(rect, 12.0, 5.0), p(rect, 12.0, 19.0), color);
            seg(painter, p(rect, 5.0, 12.0), p(rect, 19.0, 12.0), color);
        }
        Icon::Audio => {
            // speaker polygon + waves
            polyline(
                painter,
                &[
                    p(rect, 11.0, 5.0),
                    p(rect, 6.0, 9.0),
                    p(rect, 2.0, 9.0),
                    p(rect, 2.0, 15.0),
                    p(rect, 6.0, 15.0),
                    p(rect, 11.0, 19.0),
                    p(rect, 11.0, 5.0),
                ],
                color,
            );
            // arc waves
            painter.circle_stroke(p(rect, 11.0, 12.0), 4.0 * BOX / 24.0, stroke);
        }
        Icon::AudioMuted => {
            polyline(
                painter,
                &[
                    p(rect, 11.0, 5.0),
                    p(rect, 6.0, 9.0),
                    p(rect, 2.0, 9.0),
                    p(rect, 2.0, 15.0),
                    p(rect, 6.0, 15.0),
                    p(rect, 11.0, 19.0),
                    p(rect, 11.0, 5.0),
                ],
                color,
            );
            seg(painter, p(rect, 4.0, 4.0), p(rect, 20.0, 20.0), color);
        }
        Icon::Pin => {
            // pin outline
            polyline(
                painter,
                &[
                    p(rect, 12.0, 3.0),
                    p(rect, 17.5, 8.5),
                    p(rect, 12.0, 14.0),
                    p(rect, 6.5, 8.5),
                    p(rect, 12.0, 3.0),
                ],
                color,
            );
            seg(painter, p(rect, 12.0, 14.0), p(rect, 12.0, 21.0), color);
        }
        Icon::PinFilled => {
            let pts = vec![
                p(rect, 12.0, 3.0),
                p(rect, 17.5, 8.5),
                p(rect, 12.0, 14.0),
                p(rect, 6.5, 8.5),
            ];
            painter.add(egui::Shape::convex_polygon(
                pts,
                color.gamma_multiply(0.35),
                Stroke::new(STROKE, color),
            ));
            seg(painter, p(rect, 12.0, 14.0), p(rect, 12.0, 21.0), color);
        }
        Icon::Lock => {
            // rect body + shackle
            polyline(
                painter,
                &[
                    p(rect, 7.0, 11.0),
                    p(rect, 7.0, 21.0),
                    p(rect, 17.0, 21.0),
                    p(rect, 17.0, 11.0),
                    p(rect, 7.0, 11.0),
                ],
                color,
            );
            // shackle
            polyline(
                painter,
                &[
                    p(rect, 8.5, 11.0),
                    p(rect, 8.5, 7.5),
                    p(rect, 12.0, 4.0),
                    p(rect, 15.5, 7.5),
                    p(rect, 15.5, 11.0),
                ],
                color,
            );
            // keyhole dot
            painter.circle_filled(p(rect, 12.0, 16.0), 1.2, color);
        }
        Icon::Star => {
            let pts = [
                p(rect, 12.0, 3.5),
                p(rect, 14.5, 9.0),
                p(rect, 20.5, 9.5),
                p(rect, 16.2, 13.5),
                p(rect, 17.2, 19.5),
                p(rect, 12.0, 16.5),
                p(rect, 6.8, 19.5),
                p(rect, 7.8, 13.5),
                p(rect, 3.5, 9.5),
                p(rect, 9.5, 9.0),
            ];
            polyline(
                painter,
                &[
                    pts[0], pts[1], pts[2], pts[3], pts[4], pts[5], pts[6], pts[7], pts[8], pts[9],
                    pts[0],
                ],
                color,
            );
        }
        Icon::StarFilled => {
            let pts = vec![
                p(rect, 12.0, 3.5),
                p(rect, 14.5, 9.0),
                p(rect, 20.5, 9.5),
                p(rect, 16.2, 13.5),
                p(rect, 17.2, 19.5),
                p(rect, 12.0, 16.5),
                p(rect, 6.8, 19.5),
                p(rect, 7.8, 13.5),
                p(rect, 3.5, 9.5),
                p(rect, 9.5, 9.0),
            ];
            painter.add(egui::Shape::convex_polygon(
                pts,
                color.gamma_multiply(0.22),
                Stroke::new(STROKE, color),
            ));
        }
        Icon::Info => {
            painter.circle_stroke(p(rect, 12.0, 12.0), 9.0 * BOX / 24.0, stroke);
            seg(painter, p(rect, 12.0, 8.0), p(rect, 12.0, 8.0), color);
            painter.circle_filled(p(rect, 12.0, 8.2), 1.1, color);
            seg(painter, p(rect, 12.0, 11.0), p(rect, 12.0, 17.0), color);
        }
    }
}

/// Allocate a 20×20 icon centered in `rect` and paint it. Helper for custom buttons.
pub fn paint_centered(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let box_rect = Rect::from_center_size(rect.center(), Vec2::splat(BOX));
    paint(painter, box_rect, icon, color);
}

use egui::{Sense, Ui, Vec2 as EVec2};

/// Gamma-space mix of two colors (t=0 → a, t=1 → b). For hover/focus
/// transitions driven by `ctx.animate_bool_with_time`.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let aa = a.to_array();
    let bb = b.to_array();
    Color32::from_rgba_unmultiplied(
        (aa[0] as f32 + (bb[0] as f32 - aa[0] as f32) * t) as u8,
        (aa[1] as f32 + (bb[1] as f32 - aa[1] as f32) * t) as u8,
        (aa[2] as f32 + (bb[2] as f32 - aa[2] as f32) * t) as u8,
        (aa[3] as f32 + (bb[3] as f32 - aa[3] as f32) * t) as u8,
    )
}

/// Lucide icon button: `size` hit target, `icon` stroked at `fg`, background `bg`.
/// Handles hover/active via Technical UI Kit tokens (borders over shadows).
/// Hover cross-fades over ~0.12s (`animate_bool_with_time` requests repaints
/// while animating, so this plays under OnDemand rendering).
/// Returns the `Response`; caller adds `.on_hover_text(...)` and checks `.clicked()`.
#[track_caller]
pub fn button(ui: &mut Ui, size: EVec2, icon: Icon, fg: Color32, bg: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    // Stable per-call-site animation id: no signature change, no scroll
    // aliasing (unlike rect-keyed ids), no cross-talk between same-icon
    // buttons in one row (unlike icon-keyed ids).
    let hov_t = ui.ctx().animate_bool_with_time(
        egui::Id::new(std::panic::Location::caller()),
        resp.hovered(),
        0.12,
    );
    let bg_fill = if resp.is_pointer_button_down_on() {
        crate::theme::ACCENT_INTERACTIVE.gamma_multiply(0.18)
    } else {
        mix(bg, crate::theme::SURFACE_3, hov_t)
    };
    let stroke = egui::Stroke::new(
        1.0,
        mix(
            crate::theme::BORDER_SUBTLE,
            crate::theme::BORDER_ACTIVE,
            hov_t,
        ),
    );
    ui.painter()
        .rect_filled(rect, crate::theme::CORNER_SM, bg_fill);
    ui.painter().rect_stroke(
        rect,
        crate::theme::CORNER_SM,
        stroke,
        egui::StrokeKind::Inside,
    );
    // dim icon when disabled via ui.is_enabled() check
    let icon_color = if ui.is_enabled() {
        fg
    } else {
        fg.gamma_multiply(0.35)
    };
    paint_centered(ui.painter(), rect, icon, icon_color);
    resp
}

/// Smaller 28×28 icon button for dense bars (tabs).
#[track_caller]
pub fn small_button(ui: &mut Ui, icon: Icon, fg: Color32, bg: Color32) -> egui::Response {
    button(ui, EVec2::new(28.0, 28.0), icon, fg, bg)
}
