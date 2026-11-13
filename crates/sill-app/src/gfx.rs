//! The window, its GPU surface and egui: one frame is `Gfx::frame`.

use crate::video_view::{SharedMailbox, VideoRenderer};
use egui::ViewportId;
use std::num::NonZeroU32;
use std::sync::Arc;
use winit::window::Window;

pub struct Gfx {
    pub window: Arc<Window>,
    pub ctx: egui::Context,
    pub input: egui_winit::State,
    painter: egui_wgpu::winit::Painter,
    /// Texture changes a frame did not get to apply (the surface was being resized): the next one does.
    textures: egui::TexturesDelta,
}

impl Drop for Gfx {
    fn drop(&mut self) {
        self.textures.clear();
    }
}

impl Gfx {
    pub fn new(window: Arc<Window>, display: winit::event_loop::OwnedDisplayHandle, mailbox: SharedMailbox) -> Result<Self, String> {
        let ctx = egui::Context::default();
        let input = egui_winit::State::new(ctx.clone(), ViewportId::ROOT, &window, Some(window.scale_factor() as f32), window.theme(), None);
        let config = egui_wgpu::WgpuConfiguration {
            surface: egui_wgpu::SurfaceConfig::LOW_LATENCY,
            wgpu_setup: egui_wgpu::WgpuSetup::from_display_handle(display),
            ..Default::default()
        };
        let options = egui_wgpu::RendererOptions { msaa_samples: 1, dithering: false, ..Default::default() };
        let mut painter = pollster::block_on(egui_wgpu::winit::Painter::new(ctx.clone(), config, false, options));
        pollster::block_on(painter.set_window(ViewportId::ROOT, Some(window.clone()))).map_err(|e| e.to_string())?;
        let state = painter.render_state().ok_or("no GPU surface")?;
        log::info!("GPU: {}", egui_wgpu::adapter_info_summary(&state.adapter.get_info()));
        let video = VideoRenderer::new(&state.device, state.target_format, mailbox);
        state.renderer.write().callback_resources.insert(video);
        Ok(Self { window, ctx, input, painter, textures: Default::default() })
    }

    pub fn resized(&mut self, width: u32, height: u32) {
        if let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) {
            self.painter.on_window_resized(ViewportId::ROOT, w, h);
        }
    }

    /// Read the video renderer's counters (what is on the GPU now).
    pub fn video<R>(&self, f: impl FnOnce(&mut VideoRenderer) -> R) -> Option<R> {
        let state = self.painter.render_state()?;
        let mut renderer = state.renderer.write();
        renderer.callback_resources.get_mut::<VideoRenderer>().map(f)
    }

    /// One frame: run `ui`, draw it; `capture` gets the frame's pixels when given.
    pub fn frame(&mut self, capture: Option<egui::ScreenshotCallback>, ui: impl FnMut(&mut egui::Ui)) -> Option<std::time::Duration> {
        let raw = self.input.take_egui_input(&self.window);
        let out = self.ctx.run_ui(raw, ui);
        self.textures.append(out.textures_delta);
        self.input.handle_platform_output(&self.window, out.platform_output);
        let primitives = self.ctx.tessellate(out.shapes, out.pixels_per_point);
        self.painter.paint_and_update_textures(
            ViewportId::ROOT,
            out.pixels_per_point,
            [0.07, 0.07, 0.08, 1.0],
            &primitives,
            &mut self.textures,
            capture.into_iter().collect(),
            &self.window,
        );
        // When egui wants the next frame (an animation, a blinking caret); nil: when something happens.
        let delay = out.viewport_output.get(&ViewportId::ROOT).map(|v| v.repaint_delay)?;
        (delay < std::time::Duration::from_secs(3600)).then_some(delay)
    }
}
