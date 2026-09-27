//! N64 video + CRT (a spike on the range): the frame at PD's own resolution
//! (320×220 NTSC) through the RDP's 16-bit store, the VI's filters and an
//! NTSC tube. See `n64video.wgsl` for what each pass models and where each
//! number comes from.
//!
//! The engine renders the world into a low-res scene target
//! ([`engine::render::renderer::Renderer::set_scene_size`]), the guns and PD's
//! framebuffer effects draw into it as usual, and [`N64Video::run`] takes it
//! from there to the swapchain. [`N64Video::run_still`] runs only the CRT half
//! on an image, e.g. an emulator capture, so the two halves can be judged
//! separately.

use bytemuck::{Pod, Zeroable};

use super::font::Canvas;

/// PD's NTSC low-res framebuffer (`FBALLOC_WIDTH_LO` × `FBALLOC_HEIGHT_LO`,
/// `constants.h:3655`).
pub const N64_W: u32 = 320;
pub const N64_H: u32 = 220;
/// Visible raster lines (240p); PD's 220 sit centred in them.
pub const LINES: u32 = 240;

/// The framebuffer the world renders into. The first two are PD's own modes
/// (`constants.h:3655`: hi-res doubles the width only); the third is sharper
/// than any N64 and goes out on a 480-line raster.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resolution {
    Lo,
    Hi,
    Double,
}

impl Resolution {
    pub const ALL: [Resolution; 3] = [Resolution::Lo, Resolution::Hi, Resolution::Double];
    pub fn size(self) -> (u32, u32) {
        match self {
            Resolution::Lo => (N64_W, N64_H),
            Resolution::Hi => (N64_W * 2, N64_H),
            Resolution::Double => (N64_W * 2, N64_H * 2),
        }
    }
    /// Raster lines on the tube.
    pub fn lines(self) -> u32 {
        match self {
            Resolution::Double => LINES * 2,
            _ => LINES,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Resolution::Lo => "320×220 (PD)",
            Resolution::Hi => "640×220 (PD hi-res)",
            Resolution::Double => "640×440 (beyond N64)",
        }
    }
}

/// CRT settings in one go (the tube's knobs only, not the N64 stages).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preset {
    Clean,
    SVideo,
    Composite,
}

impl Preset {
    pub const ALL: [Preset; 3] = [Preset::Clean, Preset::SVideo, Preset::Composite];
    pub fn label(self) -> &'static str {
        match self {
            Preset::Clean => "clean RGB",
            Preset::SVideo => "S-Video TV",
            Preset::Composite => "composite TV",
        }
    }
    pub fn apply(self, v: &mut VideoSettings) {
        let (signal, scan, mask, hal, curve, sharp) = match self {
            Preset::Clean => (Signal::Rgb, 0.45, 0.15, 0.03, 0.03, 1.4),
            Preset::SVideo => (Signal::SVideo, 0.9, 0.25, 0.05, 0.05, 1.0),
            Preset::Composite => (Signal::Composite, 0.75, 0.35, 0.06, 0.06, 1.0),
        };
        v.signal = signal;
        v.scanlines = scan;
        v.mask = Mask::ApertureGrille;
        v.mask_strength = mask;
        v.halation = hal;
        v.curvature = curve;
        v.sharpness = sharp;
    }
}
/// Signal samples per line (4 per framebuffer pixel).
const SIG_W: u32 = 1280;
const GLOW_W: u32 = 160;
const GLOW_H: u32 = 120;

/// The TV set's photo (4:3, chroma-green screen) and the box its screen fills,
/// in the photo's pixels: the green spans x 234..1215, y 104..808, and the box
/// runs a few pixels past it so the raster is under the key's soft edge.
const TV_PHOTO: &[u8] = include_bytes!("crt_screen.png");
const TV_SIZE: [f32; 2] = [1448.0, 1086.0];
const TV_SCREEN: [f32; 4] = [231.0, 101.0, 1219.0, 812.0];

/// Where the tube goes inside a TV set drawn over `rect`.
fn tv_screen_rect(rect: [f32; 4]) -> [f32; 4] {
    let (sx, sy) = (rect[2] / TV_SIZE[0], rect[3] / TV_SIZE[1]);
    let x0 = rect[0] + TV_SCREEN[0] * sx;
    let y0 = rect[1] + TV_SCREEN[1] * sy;
    [x0.floor(), y0.floor(), ((TV_SCREEN[2] - TV_SCREEN[0]) * sx).ceil(), ((TV_SCREEN[3] - TV_SCREEN[1]) * sy).ceil()]
}

/// Near/far of the engine's world projection (`app::world_vp`) and PD's gun
/// projection (`PdRenderer::draw`), for the edge finder's 1/z.
pub const WORLD_NEAR_FAR: (f32, f32) = (0.05, 300.0);
pub const GUN_NEAR_FAR: (f32, f32) = (1.5, 1000.0);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Signal {
    Rgb,
    SVideo,
    Composite,
}

impl Signal {
    pub const ALL: [Signal; 3] = [Signal::Composite, Signal::SVideo, Signal::Rgb];
    pub fn label(self) -> &'static str {
        match self {
            Signal::Rgb => "RGB (modded)",
            Signal::SVideo => "S-Video",
            Signal::Composite => "composite",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mask {
    None,
    ApertureGrille,
    Slot,
    Shadow,
}

impl Mask {
    pub const ALL: [Mask; 4] = [Mask::ApertureGrille, Mask::Slot, Mask::Shadow, Mask::None];
    pub fn label(self) -> &'static str {
        match self {
            Mask::None => "none",
            Mask::ApertureGrille => "aperture grille",
            Mask::Slot => "slot mask",
            Mask::Shadow => "shadow mask",
        }
    }
}

/// Every stage can be switched off on its own, to see what it contributes.
#[derive(Clone, Copy, Debug)]
pub struct VideoSettings {
    /// Master switch: render at `resolution` and run the passes below.
    pub n64: bool,
    pub resolution: Resolution,
    /// The RDP's 3-point texture filter (world and guns).
    pub three_point: bool,
    /// RGBA5551 framebuffer with the RDP's Bayer dither.
    pub fb16: bool,
    /// The VI's dither ("restore") filter.
    pub dither_filter: bool,
    /// The VI's edge anti-aliasing (coverage estimated from depth).
    pub aa: bool,
    /// The VI's divot filter.
    pub divot: bool,
    /// The tube; off = the raw 240-line raster, nearest, at 4:3.
    pub crt: bool,
    pub signal: Signal,
    /// 0 = lines merge, 1 = strong gaps between them.
    pub scanlines: f32,
    pub mask: Mask,
    pub mask_strength: f32,
    /// Show the tube inside a TV set (`crt_screen.png`), its green screen
    /// replaced by the picture.
    pub tv_frame: bool,
    pub curvature: f32,
    pub halation: f32,
    pub overscan: f32,
    /// Analogue bandwidth scale (1 = the standard's): above 1 the signal is
    /// sharper than a real set, below 1 softer.
    pub sharpness: f32,
}

impl Default for VideoSettings {
    fn default() -> Self {
        let mut v = VideoSettings {
            n64: false,
            resolution: Resolution::Hi,
            three_point: true,
            fb16: true,
            dither_filter: true,
            aa: true,
            divot: true,
            crt: true,
            signal: Signal::Composite,
            scanlines: 0.75,
            mask: Mask::ApertureGrille,
            mask_strength: 0.35,
            tv_frame: true,
            curvature: 0.06,
            halation: 0.06,
            overscan: 0.08,
            sharpness: 1.0,
        };
        Preset::SVideo.apply(&mut v);
        v
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct VideoU {
    size: [f32; 4],
    flags: [u32; 4],
    depth: [f32; 4],
    crt: [f32; 4],
    crt2: [f32; 4],
    tube: [f32; 4],
    raster: [f32; 4],
    ntsc: [[f32; 4]; 65],
}

/// Gaussian low-pass σ (µs) for a −3 dB bandwidth in MHz (as in the shader).
fn sigma_us(mhz: f32) -> f32 {
    0.1325 / mhz
}

/// The composite decoder's taps (see `fs_signal`) and the luma filter's gain
/// at the subcarrier. Taps sit 1/9 of a subcarrier period apart. Luma is a
/// gaussian (~3 MHz); I and Q are a 9-tap box (one period, so its response is
/// exactly zero at 3.58 and 7.16 MHz) convolved with a gaussian, ~1.3 and
/// ~0.6 MHz overall.
/// `sharpness` scales every bandwidth except the box, which must stay one
/// period long to null the subcarrier.
fn ntsc_taps(sharpness: f32) -> ([[f32; 4]; 65], f32) {
    const FSC: f32 = 3.579545;
    let dt = 1.0 / (FSC * 9.0);
    let g = |t: f32, s: f32| (-0.5 * (t / s) * (t / s)).exp();
    let k = sharpness.max(0.1);
    let (sy, si, sq) = (sigma_us(3.0 * k), sigma_us(2.5 * k), sigma_us(0.8 * k));
    let boxed = |k: i32, s: f32| (-4..=4).map(|j| g((k - j) as f32 * dt, s)).sum::<f32>();
    let mut taps = [[0.0f32; 4]; 65];
    for (i, t) in taps.iter_mut().enumerate() {
        let k = i as i32 - 32;
        *t = [g(k as f32 * dt, sy), boxed(k, si), boxed(k, sq), 0.0];
    }
    for c in 0..3 {
        let sum: f32 = taps.iter().map(|t| t[c]).sum();
        for t in taps.iter_mut() {
            t[c] /= sum;
        }
    }
    let omega = std::f32::consts::TAU * FSC * dt;
    let gain = taps.iter().enumerate().map(|(i, t)| t[0] * ((i as i32 - 32) as f32 * omega).cos()).sum();
    (taps, gain)
}

/// The largest 4:3 rect that fits the present area right of `left` (pixels),
/// centred: `[x0, y0, w, h]`.
pub fn tube_rect(present_w: u32, present_h: u32, left: f32) -> [f32; 4] {
    let left = left.clamp(0.0, present_w as f32 * 0.5);
    let aw = present_w as f32 - left;
    let ah = present_h as f32;
    let (w, h) = if aw / ah > 4.0 / 3.0 { (ah * 4.0 / 3.0, ah) } else { (aw, aw * 3.0 / 4.0) };
    [(left + (aw - w) * 0.5).floor(), ((ah - h) * 0.5).floor(), w.floor(), h.floor()]
}

struct Tex {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
}

fn tex(device: &wgpu::Device, label: &str, w: u32, h: u32, format: wgpu::TextureFormat, usage: wgpu::TextureUsages) -> Tex {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    Tex { tex, view }
}

const FB: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The TV set's photo as raw bytes (the key works on the photo's own values).
fn tv_photo(device: &wgpu::Device, queue: &wgpu::Queue) -> Tex {
    let img = image::load_from_memory(TV_PHOTO).expect("crt_screen.png").to_rgba8();
    let (w, h) = img.dimensions();
    let t = tex(device, "n64-tv-photo", w, h, FB, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);
    queue.write_texture(
        t.tex.as_image_copy(),
        img.as_raw(),
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    t
}
const SIG: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// The per-scene-size textures.
struct Targets {
    w: u32,
    h: u32,
    fb: Tex,
    vi: Tex,
    vid: Tex,
    world_depth: Tex,
}

pub struct N64Video {
    bgl: wgpu::BindGroupLayout,
    buf: wgpu::Buffer,
    sampler: wgpu::Sampler,
    rdp: wgpu::RenderPipeline,
    vi: wgpu::RenderPipeline,
    divot: wgpu::RenderPipeline,
    flat: wgpu::RenderPipeline,
    signal: wgpu::RenderPipeline,
    glow: wgpu::RenderPipeline,
    tube: wgpu::RenderPipeline,
    frame_pipe: wgpu::RenderPipeline,
    tv_photo: Tex,
    sig: Tex,
    glow_tex: Tex,
    targets: Option<Targets>,
    hud: Option<(Tex, u32, u32)>,
    /// 1×1 stand-ins for unbound slots.
    clear: Tex,
    depth1: Tex,
    still: Option<(Tex, u32, u32)>,
    frame: u32,
    ntsc_taps: [[f32; 4]; 65],
    ntsc_gain: f32,
    /// The sharpness the taps were built for.
    ntsc_sharpness: f32,
    /// Raster lines the signal texture is sized for.
    sig_lines: u32,
}

impl N64Video {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, present_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("n64video"),
            source: wgpu::ShaderSource::Wgsl(include_str!("n64video.wgsl").into()),
        });
        let tex_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let depth_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Depth,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("n64video"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                tex_entry(1),
                tex_entry(2),
                depth_entry(3),
                depth_entry(4),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("n64video"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipe_blend = |entry: &str, format: wgpu::TextureFormat, blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_main"), buffers: &[], compilation_options: Default::default() },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        };
        let pipe = |entry: &str, format: wgpu::TextureFormat| pipe_blend(entry, format, None);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("n64video-u"),
            size: std::mem::size_of::<VideoU>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("n64video"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let sampled = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let upload = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
        N64Video {
            rdp: pipe("fs_rdp", FB),
            vi: pipe("fs_vi", FB),
            divot: pipe("fs_divot", FB),
            flat: pipe("fs_flat", present_format),
            signal: pipe("fs_signal", SIG),
            glow: pipe("fs_glow", SIG),
            tube: pipe("fs_tube", present_format),
            frame_pipe: pipe_blend("fs_frame", present_format, Some(wgpu::BlendState::ALPHA_BLENDING)),
            tv_photo: tv_photo(device, queue),
            sig: tex(device, "n64-signal", SIG_W, LINES, SIG, sampled),
            glow_tex: tex(device, "n64-glow", GLOW_W, GLOW_H, SIG, sampled),
            clear: tex(device, "n64-clear", 1, 1, FB, upload),
            depth1: tex(device, "n64-depth1", 1, 1, wgpu::TextureFormat::Depth32Float, wgpu::TextureUsages::TEXTURE_BINDING),
            bgl,
            buf,
            sampler,
            targets: None,
            hud: None,
            still: None,
            frame: 0,
            ntsc_taps: ntsc_taps(1.0).0,
            ntsc_gain: ntsc_taps(1.0).1,
            ntsc_sharpness: 1.0,
            sig_lines: LINES,
        }
    }

    /// The signal texture for `lines` raster lines, and the composite taps for
    /// `sharpness`.
    fn ensure_raster(&mut self, device: &wgpu::Device, lines: u32, sharpness: f32) {
        if self.sig_lines != lines {
            let sampled = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
            self.sig = tex(device, "n64-signal", SIG_W, lines, SIG, sampled);
            self.sig_lines = lines;
        }
        if self.ntsc_sharpness != sharpness {
            (self.ntsc_taps, self.ntsc_gain) = ntsc_taps(sharpness);
            self.ntsc_sharpness = sharpness;
        }
    }

    fn ensure_targets(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        if self.targets.as_ref().is_some_and(|t| t.w == w && t.h == h) {
            return;
        }
        let sampled = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        self.targets = Some(Targets {
            w,
            h,
            fb: tex(device, "n64-fb", w, h, FB, sampled),
            vi: tex(device, "n64-vi", w, h, FB, sampled),
            vid: tex(device, "n64-vid", w, h, FB, sampled),
            world_depth: tex(
                device,
                "n64-world-depth",
                w,
                h,
                wgpu::TextureFormat::Depth32Float,
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            ),
        });
    }

    /// The texture the world's depth should be copied into this frame (set it
    /// as `PdRenderer::world_depth_copy`'s destination).
    pub fn world_depth(&mut self, device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
        self.ensure_targets(device, w, h);
        self.targets.as_ref().unwrap().world_depth.tex.clone()
    }

    /// This frame's HUD (PD pixels, premultiplied), composited in the RDP pass.
    pub fn upload_hud(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, cv: Option<&Canvas>) {
        let Some(cv) = cv else {
            self.hud = None;
            return;
        };
        let (w, h) = (cv.w as u32, cv.h as u32);
        if !self.hud.as_ref().is_some_and(|(_, hw, hh)| *hw == w && *hh == h) {
            let t = tex(device, "n64-hud", w, h, FB, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);
            self.hud = Some((t, w, h));
        }
        let (t, _, _) = self.hud.as_ref().unwrap();
        queue.write_texture(
            t.tex.as_image_copy(),
            &cv.rgba8(),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    }

    fn uniform(&self, s: &VideoSettings, src: (u32, u32), lines: u32, rect: [f32; 4]) -> VideoU {
        let signal = match s.signal {
            Signal::Rgb => 0.0,
            Signal::SVideo => 1.0,
            Signal::Composite => 2.0,
        };
        let mask = match s.mask {
            Mask::None => 0.0,
            Mask::ApertureGrille => 1.0,
            Mask::Slot => 2.0,
            Mask::Shadow => 3.0,
        };
        VideoU {
            size: [src.0 as f32, src.1 as f32, lines as f32, self.frame as f32],
            flags: [s.fb16 as u32, s.dither_filter as u32, s.aa as u32, s.divot as u32],
            depth: [WORLD_NEAR_FAR.0, WORLD_NEAR_FAR.1, GUN_NEAR_FAR.0, GUN_NEAR_FAR.1],
            crt: [signal, s.scanlines, mask, s.mask_strength],
            crt2: [s.curvature, s.halation, s.overscan, self.ntsc_gain],
            tube: rect,
            raster: [self.sig_lines as f32, s.sharpness, (s.crt && s.tv_frame) as u32 as f32, 0.0],
            ntsc: self.ntsc_taps,
        }
    }

    fn bind(&self, device: &wgpu::Device, t0: &wgpu::TextureView, t1: &wgpu::TextureView, d0: &wgpu::TextureView, d1: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("n64video"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(t0) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(t1) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(d0) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(d1) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }

    fn pass(
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        pipe: &wgpu::RenderPipeline,
        bind: &wgpu::BindGroup,
        viewport: Option<[f32; 4]>,
    ) {
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("n64video"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        if let Some([x, y, w, h]) = viewport {
            rp.set_viewport(x, y, w.max(1.0), h.max(1.0), 0.0, 1.0);
        }
        rp.set_pipeline(pipe);
        rp.set_bind_group(0, bind, &[]);
        rp.draw(0..3, 0..1);
    }

    /// The tube's rect for `rect`: the whole of it, or the TV set's screen.
    fn tube_in(rect: [f32; 4], s: &VideoSettings) -> [f32; 4] {
        if s.crt && s.tv_frame { tv_screen_rect(rect) } else { rect }
    }

    /// The CRT half (or the flat raster) from a VI-output texture to `present`.
    fn present(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        vid: &wgpu::TextureView,
        present: &wgpu::TextureView,
        rect: [f32; 4],
        s: &VideoSettings,
    ) {
        let (d, c) = (&self.depth1.view, &self.clear.view);
        if !s.crt {
            Self::pass(encoder, present, &self.flat, &self.bind(device, vid, c, d, d), Some(rect));
            return;
        }
        Self::pass(encoder, &self.sig.view, &self.signal, &self.bind(device, vid, c, d, d), None);
        Self::pass(encoder, &self.glow_tex.view, &self.glow, &self.bind(device, &self.sig.view, c, d, d), None);
        let tube = Self::tube_in(rect, s);
        Self::pass(encoder, present, &self.tube, &self.bind(device, &self.sig.view, &self.glow_tex.view, d, d), Some(tube));
        if s.tv_frame {
            Self::pass(encoder, present, &self.frame_pipe, &self.bind(device, &self.tv_photo.view, c, d, d), Some(rect));
        }
    }

    /// The whole chain: `scene` (the low-res scene target, sRGB) and
    /// `scene_depth` (holding the gun pass's depth) through the RDP store and
    /// the VI, then the CRT or the flat raster into `rect` of `present`. Call
    /// after the guns, PD's post effects and [`Self::upload_hud`].
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        scene: &wgpu::TextureView,
        scene_depth: &wgpu::TextureView,
        size: (u32, u32),
        present: &wgpu::TextureView,
        rect: [f32; 4],
        s: &VideoSettings,
    ) {
        self.frame = self.frame.wrapping_add(1);
        self.ensure_targets(device, size.0, size.1);
        let lines = s.resolution.lines();
        self.ensure_raster(device, lines, s.sharpness);
        let u = self.uniform(s, size, size.1.min(lines), Self::tube_in(rect, s));
        queue.write_buffer(&self.buf, 0, bytemuck::bytes_of(&u));
        let t = self.targets.as_ref().unwrap();
        let hud = self.hud.as_ref().map(|(t, _, _)| &t.view).unwrap_or(&self.clear.view);
        let d = &self.depth1.view;
        let c = &self.clear.view;
        Self::pass(encoder, &t.fb.view, &self.rdp, &self.bind(device, scene, hud, &t.world_depth.view, scene_depth), None);
        Self::pass(encoder, &t.vi.view, &self.vi, &self.bind(device, &t.fb.view, c, d, d), None);
        Self::pass(encoder, &t.vid.view, &self.divot, &self.bind(device, &t.vi.view, c, d, d), None);
        self.present(device, encoder, &t.vid.view, present, rect, s);
    }

    /// Only the CRT half, on an image (raw display values, e.g. an emulator
    /// capture at 320×240 or a line-doubled 640×480): its rows are shown on
    /// min(height, 240) lines.
    #[allow(clippy::too_many_arguments)]
    pub fn run_still(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        rgba: &[u8],
        size: (u32, u32),
        present: &wgpu::TextureView,
        rect: [f32; 4],
        s: &VideoSettings,
    ) {
        self.ensure_raster(device, LINES, s.sharpness);
        if !self.still.as_ref().is_some_and(|(_, w, h)| (*w, *h) == size) {
            let t = tex(device, "n64-still", size.0, size.1, FB, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);
            self.still = Some((t, size.0, size.1));
        }
        let (t, _, _) = self.still.as_ref().unwrap();
        queue.write_texture(
            t.tex.as_image_copy(),
            rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(size.0 * 4), rows_per_image: Some(size.1) },
            wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        );
        let lines = size.1.min(LINES);
        let u = self.uniform(s, size, lines, Self::tube_in(rect, s));
        queue.write_buffer(&self.buf, 0, bytemuck::bytes_of(&u));
        let view = t.view.clone();
        self.present(device, encoder, &view, present, rect, s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The composite chroma taps reject a steady subcarrier (so a flat colour
    /// decodes without stripes): demodulating Y = 1 at any phase leaves ~0.
    #[test]
    fn composite_chroma_taps_null_the_subcarrier() {
        let (taps, gain) = ntsc_taps(1.0);
        let omega = std::f32::consts::TAU * 3.579545 / (3.579545 * 9.0);
        for phase in [0.0f32, 0.7, 1.9, 3.0] {
            let (mut i, mut q) = (0.0, 0.0);
            for (k, t) in taps.iter().enumerate() {
                let phi = (k as i32 - 32) as f32 * omega + phase;
                i += t[1] * 2.0 * phi.cos();
                q += t[2] * 2.0 * phi.sin();
            }
            assert!(i.abs() < 1e-4 && q.abs() < 1e-4, "phase {phase}: leak I {i} Q {q}");
        }
        assert!(gain > 0.0 && gain < 1.0, "luma gain at fsc {gain}");
    }
}
