//! Convert `egui` logical rects to `wry` physical rects.

use egui::Rect;

/// Convert an `egui` rect (logical points) to a `wry` rect (physical pixels).
/// Returns `None` if the rect or scale is not finite or has no area.
pub fn egui_rect_to_wry_rect(rect: Rect, scale_factor: f64) -> Option<wry::Rect> {
    if !rect.is_positive() || !rect.min.is_finite() || !rect.max.is_finite() {
        return None;
    }
    if !scale_factor.is_finite() || scale_factor <= 0.0 {
        return None;
    }

    let x = rect.min.x as f64 * scale_factor;
    let y = rect.min.y as f64 * scale_factor;
    let w = rect.width() as f64 * scale_factor;
    let h = rect.height() as f64 * scale_factor;

    if w <= 0.0 || h <= 0.0 {
        return None;
    }

    Some(wry::Rect {
        position: wry::dpi::Position::Physical(wry::dpi::PhysicalPosition::new(x as i32, y as i32)),
        size: wry::dpi::Size::Physical(wry::dpi::PhysicalSize::new(w as u32, h as u32)),
    })
}

pub fn test_rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::from_min_size(egui::Pos2::new(x, y), egui::Vec2::new(w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_logical_to_physical_with_scale() {
        let rect = test_rect(10.0, 20.0, 800.0, 600.0);
        let wry = egui_rect_to_wry_rect(rect, 2.0).unwrap();
        match wry.position {
            wry::dpi::Position::Physical(pos) => {
                assert_eq!(pos.x, 20);
                assert_eq!(pos.y, 40);
            }
            _ => panic!("expected Physical"),
        }
        match wry.size {
            wry::dpi::Size::Physical(size) => {
                assert_eq!(size.width, 1600);
                assert_eq!(size.height, 1200);
            }
            _ => panic!("expected Physical"),
        }
    }

    #[test]
    fn scale_one_is_identity() {
        let rect = test_rect(0.0, 0.0, 100.0, 50.0);
        let wry = egui_rect_to_wry_rect(rect, 1.0).unwrap();
        match wry.size {
            wry::dpi::Size::Physical(s) => assert_eq!(s.width, 100),
            _ => panic!(),
        }
    }

    #[test]
    fn zero_area_returns_none() {
        assert!(egui_rect_to_wry_rect(test_rect(0.0, 0.0, 0.0, 100.0), 1.0).is_none());
        assert!(egui_rect_to_wry_rect(test_rect(0.0, 0.0, 100.0, 0.0), 1.0).is_none());
    }

    #[test]
    fn non_finite_returns_none() {
        assert!(egui_rect_to_wry_rect(test_rect(f32::NAN, 0.0, 100.0, 100.0), 1.0).is_none());
        assert!(egui_rect_to_wry_rect(test_rect(0.0, 0.0, 100.0, 100.0), f64::NAN).is_none());
        assert!(egui_rect_to_wry_rect(test_rect(0.0, 0.0, 100.0, 100.0), 0.0).is_none());
    }

    #[test]
    fn negative_rect_returns_none() {
        let rect = Rect::from_min_max(egui::Pos2::new(100.0, 100.0), egui::Pos2::new(0.0, 0.0));
        assert!(egui_rect_to_wry_rect(rect, 1.0).is_none());
    }
}
