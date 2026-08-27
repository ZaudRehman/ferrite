//! GPU renderer: wraps `egui_wgpu::Painter`.

use std::sync::Arc;

use egui::Context;
use egui_wgpu::winit::Painter;
use egui_wgpu::{
    RendererOptions, SurfaceConfig as EguiSurfaceConfig, SurfaceErrorAction, WgpuConfiguration,
    WgpuSetup,
};
use wgpu::{CurrentSurfaceTexture, PresentMode};

use crate::ROOT_VIEWPORT;

pub(crate) const CLEAR_COLOR: [f32; 4] = [0.003, 0.003, 0.005, 1.0];

pub(crate) mod frame_metrics {
    pub fn ema(prev: f32, sample_secs: f32, alpha: f32) -> f32 {
        if prev <= 0.0 {
            sample_secs
        } else {
            prev * (1.0 - alpha) + sample_secs * alpha
        }
    }
}

pub(crate) struct GpuRenderer {
    painter: Painter,
}

impl GpuRenderer {
    pub fn new(
        window: &Arc<winit::window::Window>,
        ctx: &Context,
    ) -> crate::error::ShellResult<Self> {
        let config = WgpuConfiguration {
            surface: EguiSurfaceConfig {
                present_mode: PresentMode::AutoVsync,
                desired_maximum_frame_latency: Some(1),
            },
            wgpu_setup: WgpuSetup::from_display_handle(window.clone()),
            on_surface_status: Arc::new(|status| match status {
                CurrentSurfaceTexture::Outdated => {
                    tracing::trace!("surface outdated: scheduler will reconfigure");
                    SurfaceErrorAction::Reconfigure
                }
                CurrentSurfaceTexture::Lost => {
                    tracing::debug!("surface lost: scheduler will recreate");
                    SurfaceErrorAction::RecreateSurface
                }
                CurrentSurfaceTexture::Occluded => {
                    tracing::trace!("surface occluded: skipping frame");
                    SurfaceErrorAction::SkipFrame
                }
                other => {
                    tracing::warn!("dropping frame: {other:?}");
                    SurfaceErrorAction::SkipFrame
                }
            }),
        };

        let mut painter = pollster::block_on(Painter::new(
            ctx.clone(),
            config,
            false,
            RendererOptions {
                msaa_samples: 4,
                depth_stencil_format: None,
                dithering: true,
                predictable_texture_filtering: false,
            },
        ));

        pollster::block_on(painter.set_window(ROOT_VIEWPORT, Some(window.clone())))?;
        Ok(Self { painter })
    }

    pub fn on_window_resized(
        &mut self,
        size: winit::dpi::PhysicalSize<u32>,
        pixels_per_point: f32,
    ) {
        let Some(width) = std::num::NonZeroU32::new(size.width) else {
            tracing::debug!("ignoring zero-width resize");
            return;
        };
        let Some(height) = std::num::NonZeroU32::new(size.height) else {
            tracing::debug!("ignoring zero-height resize");
            return;
        };
        self.painter.on_window_resized(ROOT_VIEWPORT, width, height);
        let _ = pixels_per_point;
    }

    pub fn paint_frame(
        &mut self,
        window: &Arc<winit::window::Window>,
        pixels_per_point: f32,
        clipped_primitives: &[egui::ClippedPrimitive],
        textures_delta: &mut egui::TexturesDelta,
    ) -> f32 {
        self.painter.paint_and_update_textures(
            ROOT_VIEWPORT,
            pixels_per_point,
            CLEAR_COLOR,
            clipped_primitives,
            textures_delta,
            Vec::new(),
            window,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::CLEAR_COLOR;
    use super::frame_metrics::ema;

    #[test]
    fn ema_seeds_from_first_sample() {
        assert_eq!(ema(0.0, 0.016, 0.1), 0.016);
    }

    #[test]
    fn ema_blends_with_alpha() {
        let seeded = 0.016;
        let sample = 0.018;
        let next = ema(seeded, sample, 0.25);
        assert!((next - 0.0165).abs() < 1e-4);
    }

    #[test]
    fn clear_color_matches_substrate() {
        assert_eq!(CLEAR_COLOR, [0.003, 0.003, 0.005, 1.0]);
    }
}
