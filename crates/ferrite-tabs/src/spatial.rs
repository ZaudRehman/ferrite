//! Logical pixel rects converted to wry physical rects. No toolkit needed.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl LogicalRect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    fn is_valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.w.is_finite()
            && self.h.is_finite()
            && self.w > 0.0
            && self.h > 0.0
    }
}

/// None when the rect or scale is not finite or has no area.
pub fn logical_to_wry_rect(rect: LogicalRect, scale_factor: f64) -> Option<wry::Rect> {
    if !rect.is_valid() {
        return None;
    }
    if !scale_factor.is_finite() || scale_factor <= 0.0 {
        return None;
    }

    let x = rect.x * scale_factor;
    let y = rect.y * scale_factor;
    let w = rect.w * scale_factor;
    let h = rect.h * scale_factor;

    if w <= 0.0 || h <= 0.0 {
        return None;
    }

    Some(wry::Rect {
        position: wry::dpi::Position::Physical(wry::dpi::PhysicalPosition::new(x as i32, y as i32)),
        size: wry::dpi::Size::Physical(wry::dpi::PhysicalSize::new(w as u32, h as u32)),
    })
}

pub fn test_rect(x: f64, y: f64, w: f64, h: f64) -> LogicalRect {
    LogicalRect::new(x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_logical_to_physical_with_scale() {
        let rect = test_rect(10.0, 20.0, 800.0, 600.0);
        let wry = logical_to_wry_rect(rect, 2.0).unwrap();
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
        let wry = logical_to_wry_rect(rect, 1.0).unwrap();
        match wry.size {
            wry::dpi::Size::Physical(s) => assert_eq!(s.width, 100),
            _ => panic!(),
        }
    }

    #[test]
    fn zero_area_returns_none() {
        assert!(logical_to_wry_rect(test_rect(0.0, 0.0, 0.0, 100.0), 1.0).is_none());
        assert!(logical_to_wry_rect(test_rect(0.0, 0.0, 100.0, 0.0), 1.0).is_none());
    }

    #[test]
    fn non_finite_returns_none() {
        assert!(logical_to_wry_rect(test_rect(f64::NAN, 0.0, 100.0, 100.0), 1.0).is_none());
        assert!(logical_to_wry_rect(test_rect(0.0, 0.0, 100.0, 100.0), f64::NAN).is_none());
        assert!(logical_to_wry_rect(test_rect(0.0, 0.0, 100.0, 100.0), 0.0).is_none());
    }

    #[test]
    fn negative_rect_returns_none() {
        let rect = LogicalRect::new(100.0, 100.0, -100.0, -100.0);
        assert!(logical_to_wry_rect(rect, 1.0).is_none());
    }
}
