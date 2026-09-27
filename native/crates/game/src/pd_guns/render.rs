//! The spike's own GPU layer: PD gun/hand/casing models through a port of the
//! N64 display-list state (`pdgun.wgsl`) and PD's world effects (`pdfx.wgsl`),
//! drawn inside [`engine::render::renderer::Renderer::render_with_hook`].
//!
//! Pass order mirrors PD: the world effects (sparks, bullet holes, boards) are
//! depth-tested against the world; then `bgun_render` clears the z-buffer, sets
//! the gun's own projection (near 1.5, far 1000 — `vi0000aca4`), and draws the
//! beams, each hand's gun then hand model, then the casings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

use super::bgun::*;
use super::data::{self, Material};
use super::fx::{FxKind, FxVert};
use super::gset::*;
use super::model::{Cull, Model, ModelDef, NodeKind};
use super::sim::Sim;
use crate::pd_spike::pdmath::Rng;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GunVertex {
    pos: [f32; 3],
    uv: [f32; 2],
    col: [f32; 4],
    mtx: u32,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameU {
    proj: [[f32; 4]; 4],
    ambient: [f32; 4],
    diffuse: [f32; 4],
    light_dir: [f32; 4],
    lookat_x: [f32; 4],
    lookat_y: [f32; 4],
    envcol: [f32; 4],
    /// x-ray: the flat colour + alpha `obj_render` gives a prop (alpha 0 = off).
    xray: [f32; 4],
    /// x: the cloaked gun's env alpha (0 = not cloaked).
    cloak: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MaterialU {
    cc0: [u32; 4],
    ac0: [u32; 4],
    cc1: [u32; 4],
    ac1: [u32; 4],
    prim: [f32; 4],
    env: [f32; 4],
    fog: [f32; 4],
    tex: [f32; 4],
    shift: [f32; 4],
    flags: [u32; 4],
    flags2: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FxVertex {
    pos: [f32; 3],
    st: [f32; 2],
    col: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PassU {
    view_proj: [[f32; 4]; 4],
    env: [f32; 4],
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GunPipeKey {
    alpha: bool,
    zwrite: bool,
    ztest: bool,
    decal: bool,
    cull: u8, // 0 none, 1 back, 2 front, 3 both
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct FxPipeKey {
    blend: u8, // 0 alpha, 1 opaque
    zwrite: bool,
    decal: bool,
    ztest: bool,
}

struct GpuMaterial {
    bind: wgpu::BindGroup,
    alpha: bool,
    zwrite: bool,
    ztest: bool,
    decal: bool,
    cull: Option<Cull>,
}

struct GpuBatch {
    first_index: u32,
    index_count: u32,
    base_vertex: i32,
    vertex_count: u32,
    material: usize,
}

struct GpuModel {
    def: Arc<ModelDef>,
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    batches: Vec<GpuBatch>,
    materials: Vec<GpuMaterial>,
    /// CPU copies of the STARGUNFIRE batches' vertices, re-jittered per frame.
    star_verts: HashMap<usize, Vec<GunVertex>>,
}

/// One draw's frame uniform + joint palette.
struct Slot {
    frame_buf: wgpu::Buffer,
    joint_buf: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

const MAX_JOINTS: usize = 128;

pub struct PdRenderer {
    gun_shader: wgpu::ShaderModule,
    gun_layout: wgpu::PipelineLayout,
    frame_bgl: wgpu::BindGroupLayout,
    mat_bgl: wgpu::BindGroupLayout,
    gun_pipes: HashMap<GunPipeKey, wgpu::RenderPipeline>,
    fx_shader: wgpu::ShaderModule,
    fx_layout: wgpu::PipelineLayout,
    fx_pipes: HashMap<FxPipeKey, wgpu::RenderPipeline>,
    fx_draw_bgl: wgpu::BindGroupLayout,
    fx_world_pass: (wgpu::Buffer, wgpu::BindGroup),
    fx_gun_pass: (wgpu::Buffer, wgpu::BindGroup),
    fx_draws: HashMap<(FxKind, u8), wgpu::BindGroup>,
    color_format: wgpu::TextureFormat,
    depth_format: wgpu::TextureFormat,
    models: HashMap<String, GpuModel>,
    textures: HashMap<PathBuf, (wgpu::TextureView, u32, u32)>,
    samplers: HashMap<(u8, u8, bool, bool), wgpu::Sampler>,
    white: wgpu::TextureView,
    slots: Vec<Slot>,
    fx_buf: Option<(wgpu::Buffer, u64)>,
    star_rng: Rng,
    /// Framebuffer effects (`pdpost.wgsl`): pipelines by blend, the frame copy.
    post_pipes: [wgpu::RenderPipeline; 2],
    post_bgl: wgpu::BindGroupLayout,
    post_buf: wgpu::Buffer,
    post_sampler: wgpu::Sampler,
    post_src: Option<(wgpu::Texture, wgpu::TextureView, u32, u32)>,
    /// The last finished frame (`vi_get_front_buffer`), for the zoom blur.
    post_prev: Option<(wgpu::Texture, wgpu::TextureView, u32, u32)>,
    post_seed: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PostU {
    params: [f32; 4],
    zoom: [f32; 4],
}

/// Which framebuffer effects a frame wants (`lv.c:1440`–`:1480`).
#[derive(Clone, Debug, Default)]
pub struct PostFx {
    /// `bview_draw_slayer_rocket_interlace`, with the scroll offset.
    pub interlace: Option<f32>,
    /// `bview_draw_static` alpha (0..1).
    pub static_alpha: f32,
    /// `bview_draw_zoom_blur` calls this frame, in order (PD allows two):
    /// (alpha 0..1, x scale, y scale).
    pub zoom_blurs: Vec<(f32, f32, f32)>,
    /// `player_draw_fade`: colour (0..1) and alpha.
    pub fade: Option<([f32; 3], f32)>,
}

fn tex_dir() -> PathBuf {
    data::assets_dir().join("textures")
}

fn fx_dir() -> PathBuf {
    data::assets_dir().join("fx")
}

/// Box-filter mip chain of raw (non-sRGB) RGBA8 — `tex_shrink_*` in spirit.
fn mips(w: u32, h: u32, rgba: &[u8]) -> Vec<(u32, u32, Vec<u8>)> {
    let mut out = vec![(w, h, rgba.to_vec())];
    let (mut cw, mut ch) = (w, h);
    while cw > 1 || ch > 1 {
        let (pw, ph, prev) = out.last().unwrap().clone();
        cw = (cw / 2).max(1);
        ch = (ch / 2).max(1);
        let mut next = vec![0u8; (cw * ch * 4) as usize];
        for y in 0..ch {
            for x in 0..cw {
                for c in 0..4 {
                    let mut sum = 0u32;
                    let mut n = 0u32;
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let sx = (x * 2 + dx).min(pw - 1);
                            let sy = (y * 2 + dy).min(ph - 1);
                            sum += prev[((sy * pw + sx) * 4 + c) as usize] as u32;
                            n += 1;
                        }
                    }
                    next[((y * cw + x) * 4 + c) as usize] = (sum / n) as u8;
                }
            }
        }
        out.push((cw, ch, next));
    }
    out
}

fn upload(device: &wgpu::Device, queue: &wgpu::Queue, w: u32, h: u32, rgba: &[u8], mip: bool, label: &str) -> wgpu::TextureView {
    let levels = if mip { mips(w, h, rgba) } else { vec![(w, h, rgba.to_vec())] };
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // Raw values: the N64 has no gamma, so the shaders do the maths on the
        // bytes as stored and convert to linear only at the end.
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (i, (lw, lh, px)) in levels.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: i as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            px,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(lw * 4), rows_per_image: Some(*lh) },
            wgpu::Extent3d { width: *lw, height: *lh, depth_or_array_layers: 1 },
        );
    }
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

fn rgba_f(c: [u8; 4]) -> [f32; 4] {
    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, c[3] as f32 / 255.0]
}

/// N64 tile shift: 0 none, 1..10 divide, 11..15 multiply (`G_TX_SHIFT`).
fn shift_scale(s: u8) -> f32 {
    match s {
        0 => 1.0,
        1..=10 => 1.0 / (1u32 << s) as f32,
        _ => (1u32 << (16 - s as u32)) as f32,
    }
}

fn cull_code(c: Cull) -> u8 {
    match c {
        Cull::None => 0,
        Cull::Back => 1,
        Cull::Front => 2,
        Cull::Both => 3,
    }
}

impl PdRenderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, color_format: wgpu::TextureFormat, depth_format: wgpu::TextureFormat) -> Self {
        let gun_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pdgun"),
            source: wgpu::ShaderSource::Wgsl(include_str!("pdgun.wgsl").into()),
        });
        let fx_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pdfx"),
            source: wgpu::ShaderSource::Wgsl(include_str!("pdfx.wgsl").into()),
        });
        let frame_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pdgun-frame"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let tex_entries = |label| wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        };
        let mat_bgl = device.create_bind_group_layout(&tex_entries("pdgun-material"));
        // The fx layout adds a second texture: the explosion's colour map (tile 1
        // of `g_TcGdl2`), white for every other kind.
        let fx_draw_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pdfx-draw"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let pass_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pdfx-pass"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let gun_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pdgun"),
            bind_group_layouts: &[&frame_bgl, &mat_bgl],
            push_constant_ranges: &[],
        });
        let fx_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pdfx"),
            bind_group_layouts: &[&pass_bgl, &fx_draw_bgl],
            push_constant_ranges: &[],
        });
        let mk_pass = |label| {
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of::<PassU>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &pass_bgl,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() }],
            });
            (buf, bind)
        };
        let white = upload(device, queue, 1, 1, &[255, 255, 255, 255], false, "pd-white");
        let post_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pdpost"),
            source: wgpu::ShaderSource::Wgsl(include_str!("pdpost.wgsl").into()),
        });
        let post_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pdpost"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let post_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pdpost"),
            bind_group_layouts: &[&post_bgl],
            push_constant_ranges: &[],
        });
        let mk_post = |blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("pdpost"),
                layout: Some(&post_layout),
                vertex: wgpu::VertexState { module: &post_shader, entry_point: Some("vs_main"), buffers: &[], compilation_options: Default::default() },
                fragment: Some(wgpu::FragmentState {
                    module: &post_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState { format: color_format, blend, write_mask: wgpu::ColorWrites::ALL })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview: None,
                cache: None,
            })
        };
        let post_pipes = [mk_post(None), mk_post(Some(wgpu::BlendState::ALPHA_BLENDING))];
        let post_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pdpost"),
            // One 256-byte slot per pass (uniform offsets must be 256-aligned).
            size: 256 * 8,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let post_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("pdpost"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        PdRenderer {
            post_pipes,
            post_bgl,
            post_buf,
            post_sampler,
            post_src: None,
            post_prev: None,
            post_seed: 0,
            gun_shader,
            gun_layout,
            frame_bgl,
            mat_bgl,
            gun_pipes: HashMap::new(),
            fx_shader,
            fx_layout,
            fx_pipes: HashMap::new(),
            fx_draw_bgl,
            fx_world_pass: mk_pass("pdfx-world"),
            fx_gun_pass: mk_pass("pdfx-gun"),
            fx_draws: HashMap::new(),
            color_format,
            depth_format,
            models: HashMap::new(),
            textures: HashMap::new(),
            samplers: HashMap::new(),
            white,
            slots: Vec::new(),
            fx_buf: None,
            star_rng: Rng::new(99),
        }
    }

    fn sampler(&mut self, device: &wgpu::Device, cms: u8, cmt: u8, linear: bool, mip: bool) -> wgpu::Sampler {
        let key = (cms, cmt, linear, mip);
        if let Some(s) = self.samplers.get(&key) {
            return s.clone();
        }
        let mode = |m: u8| match m {
            1 => wgpu::AddressMode::ClampToEdge,
            2 => wgpu::AddressMode::MirrorRepeat,
            _ => wgpu::AddressMode::Repeat,
        };
        let f = if linear { wgpu::FilterMode::Linear } else { wgpu::FilterMode::Nearest };
        let s = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("pd-sampler"),
            address_mode_u: mode(cms),
            address_mode_v: mode(cmt),
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: f,
            min_filter: f,
            mipmap_filter: if mip { wgpu::FilterMode::Linear } else { wgpu::FilterMode::Nearest },
            lod_max_clamp: if mip { 32.0 } else { 0.0 },
            ..Default::default()
        });
        self.samplers.insert(key, s.clone());
        s
    }

    fn texture(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, path: &Path, mip: bool) -> Option<(wgpu::TextureView, u32, u32)> {
        if let Some(t) = self.textures.get(path) {
            return Some(t.clone());
        }
        let img = match image::open(path) {
            Ok(i) => i.to_rgba8(),
            Err(e) => {
                log::warn!("pd_guns: texture {}: {e}", path.display());
                return None;
            }
        };
        let (w, h) = img.dimensions();
        let view = upload(device, queue, w, h, img.as_raw(), mip, "pd-tex");
        self.textures.insert(path.to_path_buf(), (view.clone(), w, h));
        Some((view, w, h))
    }

    /// Upload every model the sim loaded (guns, hands, casings).
    pub fn load_models(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, sim: &Sim) {
        let defs: Vec<(String, Arc<ModelDef>)> = sim.models.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        // Keyed by the definition's own name (what a live `Model` carries).
        for (_, def) in defs {
            let name = def.name.clone();
            let m = self.build_model(device, queue, def);
            self.models.insert(name, m);
        }
    }

    fn build_model(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, def: Arc<ModelDef>) -> GpuModel {
        let mut verts: Vec<GunVertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let mut batches = Vec::new();
        let mut star_verts = HashMap::new();
        let star_nodes: Vec<bool> = def.nodes.iter().map(|n| matches!(n.kind, NodeKind::StarGunfire { .. })).collect();
        for (bi, b) in def.batches.iter().enumerate() {
            let base = verts.len();
            let bv: Vec<GunVertex> = b
                .verts
                .iter()
                .map(|v| GunVertex {
                    pos: [v[0], v[1], v[2]],
                    uv: [v[4], v[5]],
                    col: [v[6], v[7], v[8], v[9]],
                    mtx: v[3].max(0.0) as u32,
                    flags: v[10] as u32,
                })
                .collect();
            if star_nodes.get(b.node).copied().unwrap_or(false) {
                star_verts.insert(bi, bv.clone());
            }
            verts.extend_from_slice(&bv);
            let first = indices.len() as u32;
            indices.extend_from_slice(&b.indices);
            batches.push(GpuBatch {
                first_index: first,
                index_count: b.indices.len() as u32,
                base_vertex: base as i32,
                vertex_count: bv.len() as u32,
                material: b.material,
            });
        }
        if verts.is_empty() {
            verts.push(GunVertex::zeroed());
        }
        if indices.is_empty() {
            indices.extend_from_slice(&[0, 0, 0]);
        }
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pdgun-vb"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });
        let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pdgun-ib"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let file_textures = def.file.as_ref().map(|f| f.textures.clone()).unwrap_or_default();
        let mut materials = Vec::new();
        for m in &def.materials {
            materials.push(self.build_material(device, queue, m, &file_textures));
        }
        GpuModel { def, vbuf, ibuf, batches, materials, star_verts }
    }

    fn build_material(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        m: &Material,
        textures: &HashMap<String, data::TextureRef>,
    ) -> GpuMaterial {
        let mut view = self.white.clone();
        let mut size = [1.0f32, 1.0];
        let mut has_tex = 0.0;
        let (mut cms, mut cmt, mut linear, mut mip) = (0u8, 0u8, true, false);
        let mut shift = [1.0f32, 1.0];
        let mut ul = [0.0f32, 0.0];
        if let Some(t) = &m.texture {
            if let Some(tr) = textures.get(&t.id.to_string()) {
                let path = tex_dir().join(&tr.file);
                if let Some((v, w, h)) = self.texture(device, queue, &path, t.mipmap) {
                    view = v;
                    size = [w as f32, h as f32];
                    has_tex = 1.0;
                }
            }
            cms = t.cms;
            cmt = t.cmt;
            linear = t.linear;
            mip = t.mipmap;
            shift = [shift_scale(t.shifts), shift_scale(t.shiftt)];
            ul = [t.uls, t.ult];
        }
        let sampler = self.sampler(device, cms, cmt, linear, mip);
        let c = m.combine;
        let alpha_test = match m.alpha_test.as_str() {
            "edge" => 1,
            "threshold" => 2,
            _ => 0,
        };
        let u = MaterialU {
            cc0: [c[0] as u32, c[1] as u32, c[2] as u32, c[3] as u32],
            ac0: [c[4] as u32, c[5] as u32, c[6] as u32, c[7] as u32],
            cc1: [c[8] as u32, c[9] as u32, c[10] as u32, c[11] as u32],
            ac1: [c[12] as u32, c[13] as u32, c[14] as u32, c[15] as u32],
            prim: rgba_f(m.prim),
            env: rgba_f(m.env.unwrap_or([255; 4])),
            fog: rgba_f(m.fog.unwrap_or([255, 255, 255, 0])),
            tex: [size[0], size[1], ul[0], ul[1]],
            shift: [shift[0], shift[1], has_tex, if m.two_cycle { 1.0 } else { 0.0 }],
            flags: [alpha_test, m.fog_tint as u32, m.env.is_none() as u32, m.fog.is_none() as u32],
            flags2: [m.texgen_linear as u32, 0, (m.blend == "alpha") as u32, 0],
        };
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pdgun-mat"),
            contents: bytemuck::bytes_of(&u),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pdgun-mat"),
            layout: &self.mat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });
        GpuMaterial {
            bind,
            alpha: m.blend == "alpha",
            zwrite: m.zwrite,
            ztest: m.ztest,
            decal: m.decal,
            cull: Cull::parse(&m.cull),
        }
    }

    fn gun_pipe(&mut self, device: &wgpu::Device, key: GunPipeKey) -> &wgpu::RenderPipeline {
        if !self.gun_pipes.contains_key(&key) {
            let attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x4, 3 => Uint32, 4 => Uint32];
            let blend = if key.alpha { Some(wgpu::BlendState::ALPHA_BLENDING) } else { None };
            let cull_mode = match key.cull {
                1 => Some(wgpu::Face::Back),
                2 => Some(wgpu::Face::Front),
                _ => None,
            };
            let p = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("pdgun"),
                layout: Some(&self.gun_layout),
                vertex: wgpu::VertexState {
                    module: &self.gun_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<GunVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attrs,
                    }],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.gun_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.color_format,
                        blend,
                        write_mask: if key.cull == 3 { wgpu::ColorWrites::empty() } else { wgpu::ColorWrites::ALL },
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: self.depth_format,
                    depth_write_enabled: key.zwrite,
                    depth_compare: if key.ztest {
                        if key.decal { wgpu::CompareFunction::LessEqual } else { wgpu::CompareFunction::Less }
                    } else {
                        wgpu::CompareFunction::Always
                    },
                    stencil: Default::default(),
                    bias: if key.decal {
                        wgpu::DepthBiasState { constant: -2, slope_scale: -1.0, clamp: 0.0 }
                    } else {
                        Default::default()
                    },
                }),
                multisample: Default::default(),
                multiview: None,
                cache: None,
            });
            self.gun_pipes.insert(key, p);
        }
        &self.gun_pipes[&key]
    }

    fn fx_pipe(&mut self, device: &wgpu::Device, key: FxPipeKey) -> &wgpu::RenderPipeline {
        if !self.fx_pipes.contains_key(&key) {
            let attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x4];
            let p = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("pdfx"),
                layout: Some(&self.fx_layout),
                vertex: wgpu::VertexState {
                    module: &self.fx_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<FxVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attrs,
                    }],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.fx_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.color_format,
                        blend: if key.blend == 0 { Some(wgpu::BlendState::ALPHA_BLENDING) } else { None },
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: self.depth_format,
                    depth_write_enabled: key.zwrite,
                    depth_compare: if key.ztest { wgpu::CompareFunction::LessEqual } else { wgpu::CompareFunction::Always },
                    stencil: Default::default(),
                    bias: if key.decal {
                        wgpu::DepthBiasState { constant: -8, slope_scale: -2.0, clamp: 0.0 }
                    } else {
                        Default::default()
                    },
                }),
                multisample: Default::default(),
                multiview: None,
                cache: None,
            });
            self.fx_pipes.insert(key, p);
        }
        &self.fx_pipes[&key]
    }

    /// Bind group for an effect batch kind (texture + combiner mode).
    fn fx_bind(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, kind: FxKind) -> wgpu::BindGroup {
        let mode: u8 = match kind {
            FxKind::Beam(_) => 0,
            FxKind::Spark => 1,
            FxKind::Wallhit(_) | FxKind::Smoke | FxKind::Nbomb | FxKind::GunFire(_) => 2,
            FxKind::Flat | FxKind::XrayBg | FxKind::Xray => 3,
            FxKind::Explosion(_) => 4,
        };
        if let Some(b) = self.fx_draws.get(&(kind, mode)) {
            return b.clone();
        }
        let (texnum, texnum2, cms) = match kind {
            FxKind::Beam(t) => (Some(t), None, 0u8),
            FxKind::Spark => (Some(0x001a), None, 1),
            FxKind::Wallhit(t) => (Some(t), None, 1),
            FxKind::Flat | FxKind::XrayBg | FxKind::Xray => (None, None, 0),
            FxKind::Smoke => (Some(super::smoke::TEX_SMOKE), None, 0),
            FxKind::Nbomb => (Some(super::nbomb::TEX_NBOMBDOME), None, 0),
            FxKind::GunFire(t) => (Some(t), None, 1),
            FxKind::Explosion(i) => {
                let (a, b) = super::explosions::texture_pair(i as usize);
                (Some(a), Some(b), 1)
            }
        };
        // Model textures (the gunfire flash) live with the models, the rest
        // with the effects.
        let dir = if matches!(kind, FxKind::GunFire(_)) { tex_dir() } else { fx_dir() };
        let (view, w, h) = texnum
            .and_then(|t| self.texture(device, queue, &dir.join(format!("tex_{t:04x}.png")), false))
            .unwrap_or((self.white.clone(), 1, 1));
        let view2 = texnum2
            .and_then(|t| self.texture(device, queue, &fx_dir().join(format!("tex_{t:04x}.png")), false))
            .map_or(self.white.clone(), |t| t.0);
        let sampler = self.sampler(device, cms, cms, true, false);
        let u: [f32; 4] = [w as f32, h as f32, mode as f32, 0.0];
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pdfx-draw"),
            contents: bytemuck::cast_slice(&u),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pdfx-draw"),
            layout: &self.fx_draw_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&view2) },
            ],
        });
        self.fx_draws.insert((kind, mode), bind.clone());
        bind
    }

    fn ensure_slots(&mut self, device: &wgpu::Device, n: usize) {
        while self.slots.len() < n {
            let frame_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pdgun-frame"),
                size: std::mem::size_of::<FrameU>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let joint_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pdgun-joints"),
                size: (MAX_JOINTS * 64) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pdgun-frame"),
                layout: &self.frame_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: frame_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: joint_buf.as_entire_binding() },
                ],
            });
            self.slots.push(Slot { frame_buf, joint_buf, bind });
        }
    }

    /// The whole PD layer for one frame. `world_vp` is the engine's world
    /// view-projection (metres) — the effects depth-test against the world with it.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        aspect: f32,
        world_vp: Mat4,
        sim: &Sim,
    ) {
        let p = &sim.bgun.p;
        let view = p.world_to_screen;
        // vi_shake moves the whole picture; the caller has already shifted `world_vp`.
        let shake = Mat4::from_translation(Vec3::new(0.0, sim.vi.clip_dy(), 0.0));
        let gun_proj = shake * Mat4::perspective_rh(p.fovy.to_radians(), aspect, 1.5, 1000.0);
        let env = rgba_f(p.gunshadecol);

        // ── effect geometry (world cm) ──
        let world_fx = sim.world_fx();
        let gun_fx = sim.gun_fx();
        let overlay_fx = sim.overlay_fx();
        let mut fx_verts: Vec<FxVertex> = Vec::new();
        // (pass: 0 world, 1 gun, 2 overlay after the guns; kind; first; count)
        let mut ranges: Vec<(u8, FxKind, u32, u32)> = Vec::new();
        for (pass, batches) in [(0u8, &world_fx), (1, &gun_fx), (2, &overlay_fx)] {
            for b in batches.iter() {
                let start = fx_verts.len() as u32;
                fx_verts.extend(b.verts.iter().map(|v: &FxVert| FxVertex { pos: v.pos.to_array(), st: v.st, col: v.col }));
                ranges.push((pass, b.kind, start, fx_verts.len() as u32 - start));
            }
        }
        let need = (fx_verts.len().max(1) * std::mem::size_of::<FxVertex>()) as u64;
        if self.fx_buf.as_ref().is_none_or(|(_, cap)| *cap < need) {
            let cap = need.next_power_of_two();
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pdfx-vb"),
                size: cap,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.fx_buf = Some((buf, cap));
        }
        if !fx_verts.is_empty() {
            queue.write_buffer(&self.fx_buf.as_ref().unwrap().0, 0, bytemuck::cast_slice(&fx_verts));
        }
        let world_cm = world_vp * Mat4::from_scale(Vec3::splat(0.01));
        queue.write_buffer(&self.fx_world_pass.0, 0, bytemuck::bytes_of(&PassU { view_proj: world_cm.to_cols_array_2d(), env }));
        queue.write_buffer(&self.fx_gun_pass.0, 0, bytemuck::bytes_of(&PassU { view_proj: (gun_proj * view).to_cols_array_2d(), env }));
        let mut fx_binds = Vec::new();
        for &(_, kind, _, _) in &ranges {
            fx_binds.push(self.fx_bind(device, queue, kind));
            let key = fx_key(kind);
            self.fx_pipe(device, key);
        }

        // ── gun draws: hands then casings; world props in the world pass ──
        struct DrawReq {
            model: String,
            slot: usize,
            toggles: Option<Vec<bool>>,
            forced_cull: Option<Cull>,
            world: bool,
            /// Drawn flat in its x-ray colour: translucent, no z.
            xray: bool,
            /// The cloaked gun: translucent, z-tested, no z write (ZB_XLU_SURF).
            cloaked: bool,
        }
        let mut reqs: Vec<DrawReq> = Vec::new();
        let mut slot_data: Vec<(FrameU, Vec<Mat4>)> = Vec::new();
        let look = sim.player.look;
        let up = sim.player.up;
        let lx = look.cross(up).normalize_or_zero();
        let ly = (-look).cross(lx).normalize_or_zero();
        let b = sim.room.final_brightness();
        // Riding a Slayer rocket, the view is the rocket's: no guns. In x-ray
        // bgun_render returns at once (`bondgun.c:8191`): no guns, rockets or
        // casings either.
        let xray = sim.visionmode == super::sim::VisionMode::Xray;
        let riding = sim.visionmode == super::sim::VisionMode::SlayerRocket || xray;
        for h in 0..2 {
            let hand = &sim.bgun.hands[h];
            if !hand.visible || riding {
                continue;
            }
            let Some(gm) = &hand.gunmodel else { continue };
            let weaponnum = sim.bgun.bgun_get_weapon_num(h);
            let brighter = sim.gset.has_flag(weaponnum, WEAPONFLAG_BRIGHTER);
            // lights_set_for_room (dlights.c:303) vs var80070090 (bondgun.c:162).
            let (amb, dif, dir) = if brighter {
                (150.0, 255.0, Vec3::new(-78.0, 77.0, 46.0))
            } else {
                ((b * 0.588_235_3).floor(), b, Vec3::new(77.0, 77.0, 46.0))
            };
            // bgun_render (bondgun.c:8305): a cloaked Jo's gun goes see-through,
            // MODELRENDERCONTEXT_BONDGUN_OBJ_XLU with env alpha 65 + 0.745·alpha.
            let cloak_alpha = sim.cloak.alpha();
            let cloak = if cloak_alpha < 255 { (65.0 + (cloak_alpha as f32 * 0.745_098_05).trunc()) / 255.0 } else { 0.0 };
            let mut envcol = env;
            if weaponnum == WEAPON_MAULER {
                // colour_blend(0xff00007f, envcolour, mm_maulercharge * 50)
                let weight = (hand.matmot1 * 50.0).clamp(0.0, 255.0) / 255.0;
                let red = [1.0, 0.0, 0.0, 127.0 / 255.0];
                for i in 0..4 {
                    envcol[i] = red[i] * weight + envcol[i] * (1.0 - weight);
                }
            }
            let frame = FrameU {
                proj: gun_proj.to_cols_array_2d(),
                ambient: [amb, amb, amb, 0.0],
                diffuse: [dif, dif, dif, 0.0],
                light_dir: (dir / 127.0).extend(0.0).to_array(),
                lookat_x: lx.extend(0.0).to_array(),
                lookat_y: ly.extend(0.0).to_array(),
                envcol,
                xray: [0.0; 4],
                cloak: [cloak, 0.0, 0.0, 0.0],
            };
            let slot = slot_data.len();
            slot_data.push((frame, gm.matrices.clone()));
            let forced = if sim.gset.has_flag(weaponnum, WEAPONFLAG_DUALFLIP) {
                Some(if h == HAND_RIGHT { Cull::Back } else { Cull::Front })
            } else {
                None
            };
            reqs.push(DrawReq { model: gm.def.name.clone(), slot, toggles: Some(gm.visible.clone()), forced_cull: forced, world: false, xray: false, cloaked: cloak > 0.0 });
            if let Some(hm) = &hand.handmodel {
                reqs.push(DrawReq { model: hm.def.name.clone(), slot, toggles: Some(hm.visible.clone()), forced_cull: forced, world: false, xray: false, cloaked: cloak > 0.0 });
            }
            // STARGUNFIRE jitter (model_render_node_star_gunfire).
            self.jitter_star(queue, &gm.def.name, gm);
        }
        // The rocket loaded in the launcher (`bgun_render`, `bondgun.c:8321`),
        // drawn with the gun from the muzzle matrix.
        for (name, mats) in if riding { Vec::new() } else { sim.held_rockets() } {
            let frame = FrameU {
                proj: gun_proj.to_cols_array_2d(),
                ambient: [(b * 0.588_235_3).floor(); 4],
                diffuse: [b; 4],
                light_dir: (Vec3::new(77.0, 77.0, 46.0) / 127.0).extend(0.0).to_array(),
                lookat_x: lx.extend(0.0).to_array(),
                lookat_y: ly.extend(0.0).to_array(),
                envcol: env,
                xray: [0.0; 4],
                cloak: [0.0; 4],
            };
            let slot = slot_data.len();
            slot_data.push((frame, mats));
            reqs.push(DrawReq { model: name, slot, toggles: None, forced_cull: None, world: false, xray: false, cloaked: false });
        }
        for c in sim.casings.iter().filter(|_| !xray) {
            let Some(def) = sim.models.get(super::fx::CART_MODELS[c.model]) else { continue };
            let name = def.name.clone();
            let frame = FrameU {
                proj: gun_proj.to_cols_array_2d(),
                ambient: [(b * 0.588_235_3).floor(); 4],
                diffuse: [b; 4],
                light_dir: (Vec3::new(77.0, 77.0, 46.0) / 127.0).extend(0.0).to_array(),
                lookat_x: lx.extend(0.0).to_array(),
                lookat_y: ly.extend(0.0).to_array(),
                envcol: env,
                xray: [0.0; 4],
                cloak: [0.0; 4],
            };
            let slot = slot_data.len();
            slot_data.push((frame, vec![view * c.world_matrix()]));
            reqs.push(DrawReq { model: name, slot, toggles: None, forced_cull: None, world: false, xray: false, cloaked: false });
        }
        // The weapon objects: camera-space joints, the world's projection
        // (camera cm → clip: world_vp · 0.01 · world_to_screen⁻¹).
        let world_proj = world_vp * Mat4::from_scale(Vec3::splat(0.01)) * view.inverse();
        for (name, mats, tint) in sim.world_models() {
            let frame = FrameU {
                proj: world_proj.to_cols_array_2d(),
                ambient: [(b * 0.588_235_3).floor(); 4],
                diffuse: [b; 4],
                light_dir: (Vec3::new(77.0, 77.0, 46.0) / 127.0).extend(0.0).to_array(),
                lookat_x: lx.extend(0.0).to_array(),
                lookat_y: ly.extend(0.0).to_array(),
                envcol: env,
                xray: [0.0; 4],
                cloak: [0.0; 4],
            };
            let slot = slot_data.len();
            let frame = FrameU { xray: tint.unwrap_or([0.0; 4]), ..frame };
            slot_data.push((frame, mats.iter().map(|m| view * *m).collect()));
            reqs.push(DrawReq { model: name, slot, toggles: None, forced_cull: None, world: true, xray: tint.is_some(), cloaked: false });
        }
        self.ensure_slots(device, slot_data.len());
        for (i, (f, mats)) in slot_data.iter().enumerate() {
            queue.write_buffer(&self.slots[i].frame_buf, 0, bytemuck::bytes_of(f));
            let n = mats.len().min(MAX_JOINTS);
            let arr: Vec<[[f32; 4]; 4]> = mats[..n].iter().map(|m| m.to_cols_array_2d()).collect();
            queue.write_buffer(&self.slots[i].joint_buf, 0, bytemuck::cast_slice(&arr));
        }

        // Resolve the draw list (node order, toggles, threaded cull) before the pass.
        struct Cmd {
            model: String,
            slot: usize,
            batch: usize,
            key: GunPipeKey,
            world: bool,
        }
        let mut cmds: Vec<Cmd> = Vec::new();
        for r in &reqs {
            let Some(gm) = self.models.get(&r.model) else { continue };
            let def = gm.def.clone();
            let inst_visible = |ni: usize| -> bool {
                match &r.toggles {
                    Some(t) => {
                        let mut cur = Some(ni);
                        while let Some(i) = cur {
                            if matches!(def.nodes[i].kind, NodeKind::Toggle | NodeKind::Distance { .. }) && !t.get(i).copied().unwrap_or(true) {
                                return false;
                            }
                            cur = def.nodes[i].parent;
                        }
                        true
                    }
                    None => !def.nodes.iter().enumerate().any(|(i, n)| matches!(n.kind, NodeKind::Distance { .. }) && is_ancestor(&def, i, ni) && !is_nearest_lod(&def, i)),
                }
            };
            let mut cull = Cull::Back;
            for (ni, node) in def.nodes.iter().enumerate() {
                if node.batches.is_empty() || !inst_visible(ni) {
                    continue;
                }
                for &bi in &node.batches {
                    let batch = &gm.batches[bi];
                    let mat = &gm.materials[batch.material];
                    let c = match mat.cull {
                        Some(c) => {
                            cull = c;
                            c
                        }
                        None => cull,
                    };
                    let c = r.forced_cull.unwrap_or(c);
                    cmds.push(Cmd {
                        model: r.model.clone(),
                        slot: r.slot,
                        batch: bi,
                        key: if r.xray {
                            GunPipeKey { alpha: true, zwrite: false, ztest: false, decal: false, cull: cull_code(c) }
                        } else if r.cloaked {
                            GunPipeKey { alpha: true, zwrite: false, ztest: mat.ztest, decal: mat.decal, cull: cull_code(c) }
                        } else {
                            GunPipeKey { alpha: mat.alpha, zwrite: mat.zwrite, ztest: mat.ztest, decal: mat.decal, cull: cull_code(c) }
                        },
                        world: r.world,
                    });
                }
                if let Some(c) = node.cull_exit {
                    cull = c;
                }
            }
        }
        for c in &cmds {
            self.gun_pipe(device, c.key);
        }

        let fx_vb = self.fx_buf.as_ref().map(|(b, _)| b.clone());

        // ── world effects: depth-tested against the world ──
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pd-world-fx"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color,
                    resolve_target: None,
                    // x-ray: sky_render fills the view black (`sky.c:271`) and
                    // bg_render_scene draws only the x-ray BG (`bg.c:1005`), so
                    // the engine's world picture is cleared away.
                    ops: wgpu::Operations {
                        load: if xray { wgpu::LoadOp::Clear(wgpu::Color::BLACK) } else { wgpu::LoadOp::Load },
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations {
                        load: if xray { wgpu::LoadOp::Clear(1.0) } else { wgpu::LoadOp::Load },
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            // Opaque effects (the boards; in x-ray the BG), then the weapon
            // objects, then the translucent effects over them.
            for opaque in [true, false] {
                if let Some(vb) = &fx_vb {
                    rp.set_vertex_buffer(0, vb.slice(..));
                    rp.set_bind_group(0, &self.fx_world_pass.1, &[]);
                    for (i, &(pass, kind, start, count)) in ranges.iter().enumerate() {
                        if pass != 0 || count == 0 || matches!(kind, FxKind::Flat | FxKind::XrayBg) != opaque {
                            continue;
                        }
                        rp.set_pipeline(&self.fx_pipes[&fx_key(kind)]);
                        rp.set_bind_group(1, &fx_binds[i], &[]);
                        rp.draw(start..start + count, 0..1);
                    }
                }
                if opaque {
                    let mut cur_model: Option<&str> = None;
                    for c in cmds.iter().filter(|c| c.world) {
                        let gm = &self.models[&c.model];
                        if cur_model != Some(c.model.as_str()) {
                            rp.set_vertex_buffer(0, gm.vbuf.slice(..));
                            rp.set_index_buffer(gm.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                            cur_model = Some(c.model.as_str());
                        }
                        let batch = &gm.batches[c.batch];
                        rp.set_pipeline(&self.gun_pipes[&c.key]);
                        rp.set_bind_group(0, &self.slots[c.slot].bind, &[]);
                        rp.set_bind_group(1, &gm.materials[batch.material].bind, &[]);
                        rp.draw_indexed(batch.first_index..batch.first_index + batch.index_count, batch.base_vertex, 0..1);
                    }
                }
            }
        }

        // ── bgun_render: z cleared, the gun's projection ──
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pd-gun"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            // Beams first (bondgun.c:8235).
            if let Some(vb) = &fx_vb {
                rp.set_vertex_buffer(0, vb.slice(..));
                rp.set_bind_group(0, &self.fx_gun_pass.1, &[]);
                for (i, &(pass, kind, start, count)) in ranges.iter().enumerate() {
                    if pass != 1 || count == 0 {
                        continue;
                    }
                    rp.set_pipeline(&self.fx_pipes[&fx_key(kind)]);
                    rp.set_bind_group(1, &fx_binds[i], &[]);
                    rp.draw(start..start + count, 0..1);
                }
            }
            let mut cur_model: Option<&str> = None;
            for c in cmds.iter().filter(|c| !c.world) {
                let gm = &self.models[&c.model];
                if cur_model != Some(c.model.as_str()) {
                    rp.set_vertex_buffer(0, gm.vbuf.slice(..));
                    rp.set_index_buffer(gm.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    cur_model = Some(c.model.as_str());
                }
                let batch = &gm.batches[c.batch];
                rp.set_pipeline(&self.gun_pipes[&c.key]);
                rp.set_bind_group(0, &self.slots[c.slot].bind, &[]);
                rp.set_bind_group(1, &gm.materials[batch.material].bind, &[]);
                rp.draw_indexed(batch.first_index..batch.first_index + batch.index_count, batch.base_vertex, 0..1);
            }
            // Overlays over everything (the N-Bomb storm).
            if let Some(vb) = &fx_vb {
                rp.set_vertex_buffer(0, vb.slice(..));
                rp.set_bind_group(0, &self.fx_gun_pass.1, &[]);
                for (i, &(pass, kind, start, count)) in ranges.iter().enumerate() {
                    if pass != 2 || count == 0 {
                        continue;
                    }
                    rp.set_pipeline(&self.fx_pipes[&fx_key(kind)]);
                    rp.set_bind_group(1, &fx_binds[i], &[]);
                    rp.draw(start..start + count, 0..1);
                }
            }
        }
    }

    /// The framebuffer effects the frame wants, from the sim.
    pub fn post_fx(sim: &Sim) -> PostFx {
        use super::sim::VisionMode;
        let mut fx = PostFx { static_alpha: sim.static_alpha, ..PostFx::default() };
        if sim.visionmode == VisionMode::SlayerRocket {
            fx.interlace = Some(((sim.interval_frac * 600.0) as i32 % 12) as f32);
        }
        fx.zoom_blurs.extend(sim.xray_zoom_blur());
        if let Some((alpha, scale, fade)) = sim.boost_fx {
            fx.zoom_blurs.push((alpha, scale, scale));
            fx.fade = Some(([1.0, 1.0, 1.0], fade));
        }
        fx.zoom_blurs.truncate(2);
        fx
    }

    /// PD's framebuffer effects over the finished 3D frame (before the HUD):
    /// the Slayer interlace, the zoom blur, then static. The interlace reads
    /// this frame back and the blur the last one (PD's front buffer), so they
    /// need `color_tex` (a `COPY_SRC` surface); without it they're skipped.
    /// Substituted: PD's front buffer also holds last frame's HUD; ours is
    /// kept before the HUD is drawn.
    #[allow(clippy::too_many_arguments)]
    pub fn post(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        color_tex: Option<&wgpu::Texture>,
        color: &wgpu::TextureView,
        width: u32,
        height: u32,
        fx: PostFx,
    ) {
        self.post_seed = self.post_seed.wrapping_add(1);
        // (uniform, blended, reads this frame, reads the last frame)
        let mut passes: Vec<(PostU, bool, bool, bool)> = Vec::new();
        if let Some(offset) = fx.interlace {
            passes.push((PostU { params: [0.0, 240.0, offset, 1.0], zoom: [1.0; 4] }, false, true, false));
        }
        // lv_render's order: static (lv.c:1456), the x-ray blur (:1462), the
        // boost's blur and fade (:1478).
        if fx.static_alpha > 0.0 {
            passes.push((PostU { params: [1.0, 240.0, (self.post_seed % 997) as f32, fx.static_alpha], zoom: [1.0; 4] }, true, false, false));
        }
        for &(alpha, sx, sy) in &fx.zoom_blurs {
            if alpha > 0.0 && self.post_prev.as_ref().is_some_and(|p| p.2 == width && p.3 == height) {
                passes.push((PostU { params: [2.0, 240.0, 0.0, alpha], zoom: [sx, sy, 0.0, 0.0] }, true, false, true));
            }
        }
        if let Some((rgb, alpha)) = fx.fade {
            if alpha > 0.0 {
                passes.push((PostU { params: [3.0, 240.0, 0.0, alpha], zoom: [rgb[0], rgb[1], rgb[2], 0.0] }, true, false, false));
            }
        }
        if color_tex.is_none() {
            passes.retain(|p| !p.2 && !p.3);
        }
        let reads = passes.iter().any(|p| p.2);
        if reads {
            if let Some(tex) = color_tex {
                let fresh = !matches!(&self.post_src, Some((_, _, w, h)) if *w == width && *h == height);
                if fresh {
                    let t = device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("pdpost-src"),
                        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: self.color_format,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });
                    let v = t.create_view(&Default::default());
                    self.post_src = Some((t, v, width, height));
                }
                encoder.copy_texture_to_texture(
                    tex.as_image_copy(),
                    self.post_src.as_ref().unwrap().0.as_image_copy(),
                    wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                );
            }
        }
        let src_view = self.post_src.as_ref().map(|s| s.1.clone()).unwrap_or_else(|| self.white.clone());
        let prev_view = self.post_prev.as_ref().map(|s| s.1.clone()).unwrap_or_else(|| self.white.clone());
        let size = std::mem::size_of::<PostU>() as u64;
        for (i, (u, _, _, _)) in passes.iter().enumerate() {
            queue.write_buffer(&self.post_buf, i as u64 * 256, bytemuck::bytes_of(u));
        }
        for (i, (_, blended, _, prev)) in passes.iter().enumerate() {
            let src_view = if *prev { &prev_view } else { &src_view };
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pdpost"),
                layout: &self.post_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &self.post_buf,
                            offset: i as u64 * 256,
                            size: std::num::NonZeroU64::new(size),
                        }),
                    },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src_view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.post_sampler) },
                ],
            });
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pdpost"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            rp.set_pipeline(&self.post_pipes[*blended as usize]);
            rp.set_bind_group(0, &bind, &[]);
            rp.draw(0..3, 0..1);
        }
        // Keep this frame for the next one's zoom blur.
        if let Some(tex) = color_tex {
            let fresh = !matches!(&self.post_prev, Some((_, _, w, h)) if *w == width && *h == height);
            if fresh {
                let t = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("pdpost-prev"),
                    size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: self.color_format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let v = t.create_view(&Default::default());
                self.post_prev = Some((t, v, width, height));
            }
            encoder.copy_texture_to_texture(
                tex.as_image_copy(),
                self.post_prev.as_ref().unwrap().0.as_image_copy(),
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
        }
    }

    /// `model_render_node_star_gunfire` (`model.c:3299`): every draw, each flash
    /// quad gets a random rotation of its UV square, a random corner shift and a
    /// 0.75..1 scale.
    fn jitter_star(&mut self, queue: &wgpu::Queue, name: &str, _model: &Model) {
        let Some(gm) = self.models.get(name) else { return };
        for (&bi, base) in &gm.star_verts {
            let batch = &gm.batches[bi];
            let mut out = base.clone();
            for q in 0..(base.len() / 4) {
                let src = &base[q * 4..q * 4 + 4];
                let rand1 = ((self.star_rng.random() << 10) & 0xffff) as f32;
                let ang = rand1 / 65536.0 * std::f32::consts::TAU;
                let s4 = ang.cos() * 724.0;
                let s3 = ang.sin() * 724.0;
                let s1 = (self.star_rng.random() >> 31) as usize;
                let mult = (0x10000 - (self.star_rng.random() & 0x3fff)) as f32 / 65536.0;
                let c1 = 512.0 + s3;
                let c2 = 512.0 - s3;
                let c3 = 512.0 - s4;
                let c4 = 512.0 + s4;
                let st = [[c3, c2], [c1, c3], [c4, c1], [c2, c4]];
                for k in 0..4 {
                    let from = src[(s1 + k) % 4];
                    let d = &mut out[q * 4 + k];
                    d.pos = [from.pos[0] * mult, from.pos[1] * mult, from.pos[2] * mult];
                    d.uv = [st[k][0] / 32.0, st[k][1] / 32.0];
                }
            }
            let offset = batch.base_vertex as u64 * std::mem::size_of::<GunVertex>() as u64;
            queue.write_buffer(&gm.vbuf, offset, bytemuck::cast_slice(&out));
            let _ = batch.vertex_count;
        }
    }
}

fn fx_key(kind: FxKind) -> FxPipeKey {
    match kind {
        FxKind::Flat => FxPipeKey { blend: 1, zwrite: true, decal: false, ztest: true },
        FxKind::Wallhit(_) => FxPipeKey { blend: 0, zwrite: false, decal: true, ztest: true },
        // G_RM_AA_XLU_SURF: no z at all.
        FxKind::XrayBg | FxKind::Xray => FxPipeKey { blend: 0, zwrite: false, decal: false, ztest: false },
        _ => FxPipeKey { blend: 0, zwrite: false, decal: false, ztest: true },
    }
}

fn is_ancestor(def: &ModelDef, anc: usize, node: usize) -> bool {
    let mut cur = Some(node);
    while let Some(i) = cur {
        if i == anc {
            return true;
        }
        cur = def.nodes[i].parent;
    }
    false
}

/// For models drawn without instance toggles (casings): take the nearest LOD.
fn is_nearest_lod(def: &ModelDef, node: usize) -> bool {
    match def.nodes[node].kind {
        NodeKind::Distance { near, .. } => near <= 0.0,
        _ => true,
    }
}
