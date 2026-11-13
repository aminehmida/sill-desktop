//! The stream's picture on the GPU: the decoder's NV12 in two textures, turned into RGB by a
//! shader inside egui's own pass (an egui paint callback), so panels draw over it in one frame.

use sill_media::video::Picture;
use std::sync::{Arc, Mutex};

const SHADER: &str = r#"
struct Color { m: mat4x4<f32> }
@group(0) @binding(0) var luma: texture_2d<f32>;
@group(0) @binding(1) var chroma: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;
@group(0) @binding(3) var<uniform> color: Color;

struct Out { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

@vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: Out;
    o.pos = vec4<f32>(p.x * 2.0 - 1.0, 1.0 - p.y * 2.0, 0.0, 1.0);
    o.uv = p;
    return o;
}

@fragment fn fs(v: Out) -> @location(0) vec4<f32> {
    let y = textureSample(luma, smp, v.uv).r;
    let c = textureSample(chroma, smp, v.uv).rg;
    let rgb = (color.m * vec4<f32>(y, c.x, c.y, 1.0)).rgb;
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
"#;

/// YCbCr (as sampled, 0–1) to R'G'B', column-major with the offsets in the last column.
fn matrix(bt709: bool, full: bool) -> [f32; 16] {
    let (kr, kb) = if bt709 { (0.2126f32, 0.0722f32) } else { (0.299, 0.114) };
    let kg = 1.0 - kr - kb;
    let (ys, yo, cs) = if full { (1.0, 0.0, 1.0) } else { (255.0 / 219.0, 16.0 / 255.0, 255.0 / 224.0) };
    let rv = 2.0 * (1.0 - kr) * cs;
    let bu = 2.0 * (1.0 - kb) * cs;
    let gu = -2.0 * (1.0 - kb) * kb / kg * cs;
    let gv = -2.0 * (1.0 - kr) * kr / kg * cs;
    let co = 128.0 / 255.0;
    let off = [-yo * ys - rv * co, -yo * ys - (gu + gv) * co, -yo * ys - bu * co];
    [ys, ys, ys, 0.0, 0.0, gu, bu, 0.0, rv, gv, 0.0, 0.0, off[0], off[1], off[2], 1.0]
}

/// Where the decoder leaves its newest picture and takes shown ones back.
#[derive(Default)]
pub struct Mailbox {
    pub newest: Option<Picture>,
    pub shown: Vec<Picture>,
    /// Pictures replaced before the screen took them.
    pub skipped: u64,
    /// The stream ended: the picture on the GPU is to be forgotten.
    pub cleared: bool,
}
pub type SharedMailbox = Arc<Mutex<Mailbox>>;

struct Textures {
    size: (u32, u32),
    luma: wgpu::Texture,
    chroma: wgpu::Texture,
    bind: wgpu::BindGroup,
}

pub struct VideoRenderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    textures: Option<Textures>,
    mailbox: SharedMailbox,
    look: (bool, bool),
    /// Pictures put on the GPU so far.
    pub uploaded: u64,
    /// The size of the picture on the GPU.
    pub size: Option<(u32, u32)>,
    /// The time stamp of the picture on the GPU.
    pub pts: f64,
}

impl VideoRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, mailbox: SharedMailbox) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sill video"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let tex = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sill video"),
            entries: &[
                tex(0),
                tex(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sill video"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sill video"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), buffers: &[], compilation_options: Default::default() },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sill video"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sill video color"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { pipeline, layout, sampler, uniform, textures: None, mailbox, look: (true, false), uploaded: 0, size: None, pts: 0.0 }
    }

    /// Forget the picture (the stream ended): the view draws nothing until the next one.
    pub fn clear(&mut self) {
        self.textures = None;
        self.size = None;
    }

    fn make(&self, device: &wgpu::Device, size: (u32, u32), chroma: (u32, u32)) -> Textures {
        let tex = |label, (width, height): (u32, u32), format| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let luma = tex("sill luma", size, wgpu::TextureFormat::R8Unorm);
        let chroma = tex("sill chroma", chroma, wgpu::TextureFormat::Rg8Unorm);
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sill video"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&luma.create_view(&Default::default())) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&chroma.create_view(&Default::default())) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: self.uniform.as_entire_binding() },
            ],
        });
        Textures { size, luma, chroma, bind }
    }

    /// Puts the mailbox's newest picture on the GPU, if there is one.
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let (p, cleared) = {
            let mut m = self.mailbox.lock().unwrap();
            (m.newest.take(), std::mem::take(&mut m.cleared))
        };
        if cleared {
            self.clear();
        }
        let Some(p) = p else { return };
        let size = (p.width, p.height);
        let chroma = (p.chroma_width(), p.chroma_height());
        if self.textures.as_ref().map(|t| t.size) != Some(size) {
            self.textures = Some(self.make(device, size, chroma));
            self.look = (!p.bt709, !p.full_range); // force the matrix below
        }
        if self.look != (p.bt709, p.full_range) {
            self.look = (p.bt709, p.full_range);
            queue.write_buffer(&self.uniform, 0, bytemuck::cast_slice(&matrix(p.bt709, p.full_range)));
        }
        let t = self.textures.as_ref().unwrap();
        let write = |texture: &wgpu::Texture, data: &[u8], row: u32, (width, height): (u32, u32)| {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(height) },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
        };
        write(&t.luma, &p.y, size.0, size);
        write(&t.chroma, &p.uv, chroma.0 * 2, chroma);
        self.uploaded += 1;
        self.size = Some(size);
        self.pts = p.pts;
        let mut m = self.mailbox.lock().unwrap();
        if m.shown.len() < 3 {
            m.shown.push(p);
        }
    }
}

/// The paint callback: put it in the rect where the picture goes.
pub struct VideoPaint;

impl egui_wgpu::CallbackTrait for VideoPaint {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(r) = resources.get_mut::<VideoRenderer>() {
            r.upload(device, queue);
        }
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &egui_wgpu::CallbackResources) {
        let Some(r) = resources.get::<VideoRenderer>() else { return };
        let Some(t) = &r.textures else { return };
        pass.set_pipeline(&r.pipeline);
        pass.set_bind_group(0, &t.bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::matrix;

    fn rgb(m: [f32; 16], y: f32, u: f32, v: f32) -> [f32; 3] {
        let (y, u, v) = (y / 255.0, u / 255.0, v / 255.0);
        [0, 1, 2].map(|r| m[r] * y + m[4 + r] * u + m[8 + r] * v + m[12 + r])
    }

    #[test]
    fn video_range_white_black_and_red() {
        let m = matrix(true, false);
        for c in rgb(m, 235.0, 128.0, 128.0) { assert!((c - 1.0).abs() < 0.005) }
        for c in rgb(m, 16.0, 128.0, 128.0) { assert!(c.abs() < 0.005) }
        // BT.709 red: Y 63, Cb 102, Cr 240.
        let r = rgb(m, 63.0, 102.0, 240.0);
        assert!((r[0] - 1.0).abs() < 0.02 && r[1].abs() < 0.02 && r[2].abs() < 0.02, "{r:?}");
    }

    #[test]
    fn full_range_is_identity_on_grey() {
        for c in rgb(matrix(false, true), 128.0, 128.0, 128.0) { assert!((c - 128.0 / 255.0).abs() < 0.005) }
    }
}
