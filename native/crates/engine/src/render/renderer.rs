//! wgpu renderer (Vulkan backend). Phase 1 scope: one forward pipeline with a
//! depth buffer and a single camera uniform, drawing per-region meshes that can
//! be replaced live as brushes are edited. The camera is external (a
//! [`crate::render::camera::FlyCamera`]); the renderer just consumes a view-proj matrix.

use std::collections::HashMap;
use std::sync::Arc;

use glam::Mat4;
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::assets::textured_model::TexturedModel;
use crate::render::mesh::{
    ColorVertex, ColoredMesh, CpuMesh, GpuMesh, SkinVertex, TexVertex, TexturedMesh, Vertex,
    ZoneGroup,
};
use crate::skeletal::gltf_skin::SkinnedModel;
use crate::render::textures;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Max joints in the skinned-character uniform. The GoldenEye skeleton is 15
/// bones; 16 leaves headroom and keeps the array 16-aligned. Must match
/// `shader_skinned.wgsl`'s `MAX_JOINTS`.
const MAX_JOINTS: usize = 16;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
}

/// Screen-overlay tint (rgba), multiplied onto the sampled texture in
/// `shader_screen.wgsl`. Used for the health-HUD opacity, the red damage flash,
/// and the death dimmer.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct TintUniform {
    color: [f32; 4],
}

/// Per-material uniform: `params.x` = the tile-unit → texture-space repeat scale
/// (JS `texture.repeat`). A vec4 (16 bytes) to match the WGSL std140 layout.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct MaterialUniform {
    params: [f32; 4],
}

/// Per-character uniform: world placement + the joint (skinning) matrices.
/// std140-compatible — mat4 arrays are 16-byte aligned. Matches `Char` in
/// `shader_skinned.wgsl`.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CharUniform {
    model: [[f32; 4]; 4],
    joints: [[[f32; 4]; 4]; MAX_JOINTS],
    /// `[0]` = whole-character opacity (death fade); `[1..]` is std140 padding.
    /// (Blood/damage tint is a per-vertex color in a second vertex buffer, not here.)
    opacity: [f32; 4],
}

impl Default for CharUniform {
    fn default() -> Self {
        CharUniform {
            model: Mat4::IDENTITY.to_cols_array_2d(),
            joints: [Mat4::IDENTITY.to_cols_array_2d(); MAX_JOINTS],
            opacity: [1.0, 0.0, 0.0, 0.0],
        }
    }
}

/// One primitive of the skinned character: an index range + its base-color
/// texture bind group.
struct GpuPrimitive {
    index_start: u32,
    index_count: u32,
    tex_bind: wgpu::BindGroup,
}

/// A GPU-resident skinned character's shared geometry: vertex/index buffers +
/// per-texture primitives + the decoded textures. One is uploaded (all hunters
/// share the same GLB); the per-instance pose lives in [`GpuCharacterInstance`].
struct GpuCharacterMesh {
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    primitives: Vec<GpuPrimitive>,
    /// Vertex count — sizes each instance's per-vertex blood-color buffer.
    vertex_count: u32,
    _textures: Vec<wgpu::Texture>,
}

/// One drawn instance of the shared character mesh: its own joint/model/opacity
/// uniform + its own per-vertex blood-color buffer (both rewritten each frame).
/// Pooled + reused across frames so N hunters draw the one [`GpuCharacterMesh`] N
/// times with distinct poses and independent accumulated blood.
struct GpuCharacterInstance {
    uniform_buf: wgpu::Buffer,
    uniform_bind: wgpu::BindGroup,
    /// Per-vertex RGB damage/blood color (second vertex buffer). White = clean.
    color_buf: wgpu::Buffer,
}

/// A GPU-resident enemy weapon's shared geometry (gun or muzzle-flash): the same
/// as a [`GpuViewModel`] minus the clip uniform, so one mesh can be drawn at many
/// transforms (dual-wield, or several hunters holding the same gun). The transforms
/// come from a pooled [`GpuClip`].
struct GpuWeaponMesh {
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    primitives: Vec<GpuPrimitive>,
    _textures: Vec<wgpu::Texture>,
}

/// A pooled clip-matrix uniform (`view_proj · world`) + its bind group, reused
/// frame-to-frame so a variable number of enemy weapon draws each get their own
/// transform without reallocating buffers.
struct GpuClip {
    clip_buf: wgpu::Buffer,
    clip_bind: wgpu::BindGroup,
}

/// A GPU-resident weapon viewmodel (the first-person gun): shared vertex/index
/// buffers split into per-texture primitives, plus a clip-matrix uniform
/// (rewritten each frame as the gun's overlay transform animates). Drawn in the
/// depth-cleared overlay pass.
struct GpuViewModel {
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    primitives: Vec<GpuPrimitive>,
    clip_buf: wgpu::Buffer,
    clip_bind: wgpu::BindGroup,
    _textures: Vec<wgpu::Texture>,
}

/// A region's textured GPU mesh: vertex + index buffers and the per-(scheme,zone)
/// draw groups. Scheme is carried per group (via the owning brush), so one region
/// can mix schemes across rooms.
struct TexturedRegion {
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    index_count: u32,
    groups: Vec<ZoneGroup>,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    /// Checkerboard "grid" view pipeline (TexVertex layout; ignores UV).
    pipeline: wgpu::RenderPipeline,
    /// Textured view pipeline — samples the per-zone BMP × directional light.
    textured_pipeline: wgpu::RenderPipeline,
    camera_buf: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    /// The group(0) camera/clip uniform layout, kept for building per-frame bind
    /// groups that reuse it (e.g. the viewmodel's clip matrix).
    camera_layout: wgpu::BindGroupLayout,
    depth_view: wgpu::TextureView,

    /// One classified, per-zone-grouped GPU mesh per CSG region (+ the reserved
    /// structures mesh), replaced in place on every edit.
    regions: HashMap<u32, TexturedRegion>,

    /// `materials[scheme][zone]` → the texture+sampler+repeat bind group for that
    /// zone, or `None` when the scheme doesn't define the zone. Built once at init.
    materials: Vec<[Option<wgpu::BindGroup>; 8]>,
    /// Keeps the GPU textures + per-material uniform buffers alive for the bind
    /// groups above (never read directly).
    _material_keepalive: Vec<wgpu::Texture>,
    _material_buffers: Vec<wgpu::Buffer>,
    /// `true` = checkerboard grid view; `false` = textured. Toggled by Backslash.
    grid_mode: bool,

    // Selection highlight (world-space quad over the picked face).
    highlight_pipeline: wgpu::RenderPipeline,
    highlight_mesh: Option<GpuMesh>,

    // Pending-stair ghost (translucent step preview). Same look as the highlight
    // but depth-test disabled, so it shows *through* the wall the stair carves
    // into (the steps sit behind the wall until confirmed).
    stair_ghost_pipeline: wgpu::RenderPipeline,
    stair_ghost_mesh: Option<GpuMesh>,

    // Dynamic entities (the hunter) — opaque, solid-colored.
    entity_pipeline: wgpu::RenderPipeline,
    entity_mesh: Option<GpuMesh>,

    // Skinned character (B1: one bind-pose character; later the enemy roster).
    skinned_pipeline: wgpu::RenderPipeline,
    char_tex_layout: wgpu::BindGroupLayout,
    char_uniform_layout: wgpu::BindGroupLayout,
    char_sampler: wgpu::Sampler,
    /// The shared skinned-character geometry (uploaded once), and a reused pool of
    /// per-instance pose uniforms — `character_instance_count` of them are drawn
    /// this frame (one per hunter, or the single BUILD demo).
    character_mesh: Option<GpuCharacterMesh>,
    character_instances: Vec<GpuCharacterInstance>,
    character_instance_count: usize,
    /// Texture bind-group layout for the viewmodel/muzzle/enemy-weapon meshes:
    /// base color + sampler + emissive (see `build_gpu_viewmodel`).
    viewmodel_tex_layout: wgpu::BindGroupLayout,

    // First-person weapon viewmodel (Player Combat P1): the gun, drawn in a
    // depth-cleared overlay pass so it's always on top and never clips walls.
    // Reuses the camera bind-group layout (group0 = clip matrix) + the character
    // texture layout/sampler (group1 = base-color texture).
    viewmodel_pipeline: wgpu::RenderPipeline,
    viewmodel: Option<GpuViewModel>,
    /// Whether to draw the uploaded viewmodel this frame (set per frame — the gun
    /// is uploaded once but only shown in HUNT).
    viewmodel_visible: bool,

    // Muzzle flash (Player Combat P2): a separate GLB drawn additively on top of
    // the gun in the overlay pass, only while a shot's flash is active.
    muzzle_pipeline: wgpu::RenderPipeline,
    muzzle: Option<GpuViewModel>,
    muzzle_visible: bool,

    // Enemy weapons + muzzles (A3, arsenal): each hunter's gun(s) attached to its
    // hand bone(s). Same textured GLBs as the player guns, but drawn in the FORWARD
    // pass (world-space, depth-tested against the scene) rather than the overlay —
    // reusing the viewmodel/muzzle pipelines with a `view_proj · world` clip matrix.
    // A weapon-name-keyed mesh library (uploaded once for the whole arsenal) plus a
    // pooled set of clip uniforms, so any number of guns (incl. dual-wield, and
    // several hunters sharing a gun) can be drawn each frame.
    enemy_weapon_meshes: HashMap<&'static str, GpuWeaponMesh>,
    enemy_muzzle_meshes: HashMap<&'static str, GpuWeaponMesh>,
    enemy_weapon_clips: Vec<GpuClip>,
    enemy_muzzle_clips: Vec<GpuClip>,
    /// This frame's draws as `(clip pool index, weapon-name mesh key)`.
    enemy_weapon_draws: Vec<(usize, &'static str)>,
    enemy_muzzle_draws: Vec<(usize, &'static str)>,

    // Hit sparks (Player Combat P2): bright per-vertex-colored markers at shot
    // impact points. Reuses the gizmo shader (unlit color) but depth-TESTED (so
    // sparks are occluded by geometry, unlike the always-on-top gizmo). Rebuilt
    // each frame from the live spark set.
    spark_pipeline: wgpu::RenderPipeline,
    spark_mesh: Option<GpuMesh>,

    // Explosion fireballs (explosives): additive camera-facing textured billboards
    // sampling the baked GoldenEye fireball atlas. Depth-tested (occluded by walls)
    // but not depth-writing. Mesh rebuilt each frame from the live blasts.
    blast_pipeline: wgpu::RenderPipeline,
    blast_atlas_bind: wgpu::BindGroup,
    blast_mesh: Option<GpuMesh>,

    // Breakable door panels — opaque brown; combined mesh, cleared on breach.
    door_pipeline: wgpu::RenderPipeline,
    door_mesh: Option<GpuMesh>,

    // Platform gizmo — unlit per-vertex-colored handles, drawn always-on-top
    // (depth-test disabled) so the move arrows / scale handles stay visible.
    gizmo_pipeline: wgpu::RenderPipeline,
    gizmo_mesh: Option<GpuMesh>,

    // Screen-space crosshair (the textured red GoldenEye reticle).
    crosshair_pipeline: wgpu::RenderPipeline,
    overlay_buf: wgpu::Buffer,
    overlay_bind_group: wgpu::BindGroup,
    /// The crosshair texture bind group (group 1). The texture + sampler are kept
    /// alive alongside it.
    crosshair_bind: wgpu::BindGroup,
    _crosshair_tex: wgpu::Texture,
    _crosshair_sampler: wgpu::Sampler,
    /// Whether to draw the crosshair this frame (shown only while aiming / in BUILD).
    crosshair_visible: bool,

    // Screen-space HUD text (the ammo counter; later health etc.). The pipeline +
    // sampler are fixed; the glyph atlas is uploaded once after `new()` and the
    // quad mesh is rebuilt each frame from the current HUD state.
    hud_pipeline: wgpu::RenderPipeline,
    hud_sampler: wgpu::Sampler,
    /// The glyph-atlas bind group (group 0), `None` until [`Self::upload_hud_atlas`].
    hud_atlas_bind: Option<wgpu::BindGroup>,
    _hud_atlas_tex: Option<wgpu::Texture>,
    /// This frame's HUD quads: (vertex buffer, vertex count). `None` = nothing to draw.
    hud_mesh: Option<(wgpu::Buffer, u32)>,

    // Full-screen overlays (P5): the radial health HUD (a dynamic RGBA texture),
    // the red damage flash, and the death dimmer — all one pipeline (fullscreen
    // quad × a tint), drawn in the overlay pass. group0 = texture (reuses
    // `char_tex_layout`), group1 = tint (rgba).
    screen_pipeline: wgpu::RenderPipeline,
    screen_sampler: wgpu::Sampler,
    /// Kept alive for the tint bind groups (not referenced after construction).
    _tint_layout: wgpu::BindGroupLayout,
    /// 1×1 white texture bind group — the solid-fill source for flash + death.
    white_screen_bind: wgpu::BindGroup,
    _white_screen_tex: wgpu::Texture,
    /// The radial-health texture bind group + its dims, updated when health changes.
    health_screen_bind: Option<wgpu::BindGroup>,
    _health_tex: Option<wgpu::Texture>,
    health_dims: (u32, u32),
    /// Per-overlay tint buffers + bind groups (health opacity / flash / death).
    health_tint_buf: wgpu::Buffer,
    health_tint_bind: wgpu::BindGroup,
    flash_tint_buf: wgpu::Buffer,
    flash_tint_bind: wgpu::BindGroup,
    /// The death dimmer's tint is a constant, so its buffer is write-once (keepalive).
    _death_tint_buf: wgpu::Buffer,
    death_tint_bind: wgpu::BindGroup,
    health_visible: bool,
    flash_visible: bool,
    death_visible: bool,
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct OverlayUniform {
    aspect_fix: f32,
    offset_x: f32,
    offset_y: f32,
    /// 0.0 = textured reticle (HUNT free-aim), 1.0 = small white cross (BUILD).
    mode: f32,
}

impl Renderer {
    pub async fn new(window: Arc<Window>) -> Renderer {
        let size = window.inner_size();

        let backends = pick_backends();
        log::info!("requesting backend(s): {backends:?}");
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });
        let surface = instance
            .create_surface(window.clone())
            .expect("create wgpu surface");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("request adapter");
        log::info!("adapter: {:?}", adapter.get_info());

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("engine-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::default(),
                },
                None,
            )
            .await
            .expect("request device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: pick_present_mode(&caps.present_modes),
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            // 1 = lowest input-to-photon latency (don't let the GPU queue ahead).
            desired_maximum_frame_latency: 1,
        };
        log::info!("present mode: {:?}", config.present_mode);
        surface.configure(&device, &config);

        // Camera uniform + bind group.
        let camera_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera-uniform"),
            contents: bytemuck::cast_slice(&[CameraUniform {
                view_proj: Mat4::IDENTITY.to_cols_array_2d(),
            }]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera-bg"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buf.as_entire_binding(),
            }],
        });

        // Pipeline.
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("forward-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shader.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("forward-layout"),
            bind_group_layouts: &[&camera_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("forward-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[TexVertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // Culling off: some region geometry (stairs, structures) is
                // single-winding and must show from both sides.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── Textured pipeline + per-(scheme,zone) materials. A material bind
        // group at group(1) supplies the zone's texture, a shared repeat-wrap
        // sampler, and its repeat scale. Same camera layout at group(0).
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("material-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("texture-sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let textured_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("textured-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shader_textured.wgsl").into()),
        });
        let textured_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("textured-layout"),
            bind_group_layouts: &[&camera_layout, &material_layout],
            push_constant_ranges: &[],
        });
        let textured_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("textured-pipeline"),
            layout: Some(&textured_layout),
            vertex: wgpu::VertexState {
                module: &textured_shader,
                entry_point: Some("vs_main"),
                buffers: &[TexVertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &textured_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let (materials, material_keepalive, material_buffers) =
            build_materials(&device, &queue, &material_layout, &sampler);

        // ── Highlight pipeline: translucent quad over the selected face.
        // Shares the camera bind group; blends; depth-tests but doesn't write,
        // with a small bias so it sits in front of the coplanar wall.
        let highlight_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("highlight-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shader_highlight.wgsl").into()),
        });
        let highlight_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("highlight-pipeline"),
            layout: Some(&pipeline_layout), // same layout: camera bind group only
            vertex: wgpu::VertexState {
                module: &highlight_shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &highlight_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None, // visible from either side
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: Default::default(),
                bias: wgpu::DepthBiasState {
                    constant: -1,
                    slope_scale: -1.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── Stair-ghost pipeline: the highlight shader, but depth-test disabled
        // (Always) so the pending steps preview *through* the wall they carve
        // into. Otherwise the ghost would be hidden behind solid geometry.
        let stair_ghost_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("stair-ghost-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &highlight_shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &highlight_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always, // x-ray through walls
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── Entity pipeline: opaque solid-color props (hunter). Same camera
        // layout + vertex layout as geometry; normal depth-test/write.
        let entity_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("entity-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shader_entity.wgsl").into()),
        });
        let entity_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("entity-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &entity_shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &entity_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── Skinned-character pipeline. group(0)=camera, group(1)=base-color
        // texture+sampler (per primitive), group(2)=per-character joint/model
        // uniform. Unlit (no normals in the assets); normal depth test/write.
        let char_tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("char-tex-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        // Viewmodel texture layout: base color (0) + sampler (1) + emissive (2).
        // The extra emissive slot vs `char_tex_layout` is what lets the shiny-metal
        // guns (`*EnvMapping*` materials) add their sheen — see `shader_viewmodel`.
        // Non-emissive primitives bind a 1×1 black texture there.
        let viewmodel_tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("viewmodel-tex-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
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
        let char_uniform_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("char-uniform-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // VERTEX skins with the joint matrices; FRAGMENT reads `opacity`
                    // for the death fade — so the uniform must be visible to both.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        // N64 look: crisp texels (Nearest) + clamp (materials are `*ClampS`).
        let char_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("char-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let skinned_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("skinned-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shader_skinned.wgsl").into()),
        });
        let skinned_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("skinned-layout"),
            bind_group_layouts: &[&camera_layout, &char_tex_layout, &char_uniform_layout],
            push_constant_ranges: &[],
        });
        let skinned_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("skinned-pipeline"),
            layout: Some(&skinned_layout),
            vertex: wgpu::VertexState {
                module: &skinned_shader,
                entry_point: Some("vs_main"),
                // Buffer 0 = shared geometry; buffer 1 = per-instance blood colors.
                buffers: &[SkinVertex::LAYOUT, SkinVertex::BLOOD_LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &skinned_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    // Alpha-blend so the death fade works (Track A). At opacity 1
                    // (the normal case) src-alpha 1 makes this identical to an
                    // opaque REPLACE; only the 2 s death fade is translucent.
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // Character materials are doubleSided; culling off matches.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── Viewmodel pipeline (Player Combat P1): the first-person gun. Unlit
        // textured (TexVertex). group(0)=clip matrix (camera layout), group(1)=
        // base-color texture (char layout). Depth test+write ON so the gun's own
        // parts self-occlude correctly — but it's drawn in a separate pass whose
        // depth is CLEARED, so it never tests against (clips into) world geometry.
        let viewmodel_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("viewmodel-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shader_viewmodel.wgsl").into()),
        });
        let viewmodel_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewmodel-layout"),
            bind_group_layouts: &[&camera_layout, &viewmodel_tex_layout],
            push_constant_ranges: &[],
        });
        let viewmodel_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewmodel-pipeline"),
            layout: Some(&viewmodel_layout),
            vertex: wgpu::VertexState {
                module: &viewmodel_shader,
                entry_point: Some("vs_main"),
                buffers: &[TexVertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &viewmodel_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None, // weapon materials are doubleSided
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── Door pipeline: same layout as entities, brown fragment shader.
        let door_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("door-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shader_door.wgsl").into()),
        });
        let door_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("door-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &door_shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &door_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── Gizmo pipeline: unlit, per-vertex color, drawn always-on-top
        // (depth-test disabled + no depth write) so the move/scale handles are
        // never hidden by the geometry they sit on. Same camera layout.
        let gizmo_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gizmo-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/gizmo.wgsl").into()),
        });
        let gizmo_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("gizmo-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &gizmo_shader,
                entry_point: Some("vs_main"),
                buffers: &[ColorVertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &gizmo_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── Muzzle-flash pipeline (Player Combat P2): same layout/shader as the
        // viewmodel, but ADDITIVE blend + no depth write (JS `AdditiveBlending`,
        // `depthWrite=false`, `DoubleSide`). It still depth-TESTS (`LessEqual`, like
        // three.js's default `depthTest=true`) so the gun — drawn first, writing
        // depth — OCCLUDES the parts of the flash behind the barrel/slide, instead
        // of the flash painting over the gun. The additive blend keeps it a glow.
        let muzzle_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("muzzle-pipeline"),
            layout: Some(&viewmodel_layout),
            vertex: wgpu::VertexState {
                module: &viewmodel_shader,
                entry_point: Some("vs_main"),
                buffers: &[TexVertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &viewmodel_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── Spark pipeline (Player Combat P2): hit-impact markers. Gizmo shader
        // (unlit per-vertex color, camera layout) but depth-TESTED + writing, so
        // sparks sit correctly in the scene (occluded by nearer geometry).
        let spark_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("spark-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &gizmo_shader,
                entry_point: Some("vs_main"),
                buffers: &[ColorVertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &gizmo_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── Explosion fireball pipeline (explosives): additive camera-facing textured
        // billboards. group(0)=camera view_proj (quads are built in world space, so no
        // per-instance basis needed); group(1)=the baked fireball atlas (reuses
        // char_tex_layout: texture + sampler). Additive + depth-test/no-write mirrors
        // the muzzle-flash pipeline, so the glow layers correctly over the scene.
        let billboard_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("billboard-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shader_billboard.wgsl").into()),
        });
        let (atlas_w, atlas_h, atlas_rgba) = load_explosion_atlas_rgba();
        let atlas_tex = upload_rgba_srgb(&device, &queue, atlas_w, atlas_h, &atlas_rgba, "explosion-atlas");
        let atlas_view = atlas_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("explosion-atlas-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let blast_atlas_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("explosion-atlas-bg"),
            layout: &char_tex_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&atlas_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&atlas_sampler) },
            ],
        });
        let billboard_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("billboard-layout"),
            bind_group_layouts: &[&camera_layout, &char_tex_layout],
            push_constant_ranges: &[],
        });
        let blast_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blast-pipeline"),
            layout: Some(&billboard_layout),
            vertex: wgpu::VertexState {
                module: &billboard_shader,
                entry_point: Some("vs_main"),
                buffers: &[TexVertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &billboard_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: false,
                // Depth compare ALWAYS (no occlusion) — the GoldenEye approach for
                // effect sprites: composite the fireball ON TOP of the scene instead
                // of occlusion-clipping the flat billboard against adjacent walls/
                // floors (which slices it). Additive + no depth-write keeps it a glow.
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── Crosshair pipeline: screen-space `+`, no camera, no depth test.
        let overlay_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("overlay-uniform"),
            contents: bytemuck::cast_slice(&[OverlayUniform {
                aspect_fix: config.height as f32 / config.width.max(1) as f32,
                offset_x: 0.0,
                offset_y: 0.0,
                mode: 0.0,
            }]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let overlay_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                // Vertex reads offset/aspect/size; fragment reads `mode` (which
                // crosshair style to draw), so the uniform is visible to both.
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let overlay_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("overlay-bg"),
            layout: &overlay_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: overlay_buf.as_entire_binding(),
            }],
        });
        let crosshair_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("crosshair-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shader_crosshair.wgsl").into()),
        });
        // Crosshair texture (the red reticle). group(1) reuses the char texture
        // layout (texture + sampler). Loaded from the runtime asset dir; a magenta
        // 2×2 fallback makes a missing/failed load obvious rather than invisible.
        let (ch_w, ch_h, ch_rgba) = load_crosshair_rgba();
        let crosshair_tex = upload_rgba_srgb(&device, &queue, ch_w, ch_h, &ch_rgba, "crosshair");
        let crosshair_tex_view = crosshair_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let crosshair_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("crosshair-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let crosshair_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("crosshair-bg"),
            layout: &char_tex_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&crosshair_tex_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&crosshair_sampler),
                },
            ],
        });

        let crosshair_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("crosshair-layout"),
            bind_group_layouts: &[&overlay_layout, &char_tex_layout],
            push_constant_ranges: &[],
        });
        let crosshair_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("crosshair-pipeline"),
            layout: Some(&crosshair_layout),
            vertex: wgpu::VertexState {
                module: &crosshair_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &crosshair_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── HUD pipeline: screen-space textured quads (the ammo counter and later
        // HUD text), sampling a code-defined glyph atlas. Positions are already in
        // NDC (built CPU-side each frame), so no camera/uniform — just the atlas
        // texture (group 0, reusing `char_tex_layout`). Alpha-blended, no depth,
        // drawn last in the overlay pass. The atlas + mesh are set after `new()`.
        let hud_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hud-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shader_hud.wgsl").into()),
        });
        let hud_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("hud-sampler"),
            mag_filter: wgpu::FilterMode::Nearest, // crisp pixel-font blocks
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let hud_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hud-layout"),
            bind_group_layouts: &[&char_tex_layout],
            push_constant_ranges: &[],
        });
        let hud_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hud-pipeline"),
            layout: Some(&hud_layout),
            vertex: wgpu::VertexState {
                module: &hud_shader,
                entry_point: Some("vs_main"),
                buffers: &[crate::render::mesh::HudVertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &hud_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── Full-screen overlay pipeline (P5): fullscreen quad × tint. group0 =
        // texture (char_tex_layout), group1 = tint (rgba). Alpha-blended, no depth.
        let screen_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("screen-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shader_screen.wgsl").into()),
        });
        let screen_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("screen-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let tint_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tint-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let screen_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("screen-layout"),
            bind_group_layouts: &[&char_tex_layout, &tint_layout],
            push_constant_ranges: &[],
        });
        let screen_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("screen-pipeline"),
            layout: Some(&screen_layout),
            vertex: wgpu::VertexState {
                module: &screen_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &screen_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        // 1×1 white source for the solid-fill overlays (flash + death).
        let white_screen_tex =
            upload_rgba_srgb(&device, &queue, 1, 1, &[255, 255, 255, 255], "screen-white");
        let white_view = white_screen_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let white_screen_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("screen-white-bg"),
            layout: &char_tex_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&white_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&screen_sampler) },
            ],
        });
        // Tint buffers + bind groups (initialized transparent; written per frame).
        let make_tint = |label: &str, color: [f32; 4]| {
            let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(&[TintUniform { color }]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &tint_layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() }],
            });
            (buf, bind)
        };
        let (health_tint_buf, health_tint_bind) = make_tint("health-tint", [1.0, 1.0, 1.0, 0.0]);
        let (flash_tint_buf, flash_tint_bind) = make_tint("flash-tint", [1.0, 0.0, 0.0, 0.0]);
        let (death_tint_buf, death_tint_bind) = make_tint("death-tint", [0.0, 0.0, 0.0, 0.85]);

        let depth_view = create_depth(&device, config.width, config.height);

        Renderer {
            surface,
            device,
            queue,
            config,
            pipeline,
            textured_pipeline,
            camera_buf,
            camera_bind_group,
            camera_layout,
            depth_view,
            regions: HashMap::new(),
            materials,
            _material_keepalive: material_keepalive,
            _material_buffers: material_buffers,
            grid_mode: false,
            highlight_pipeline,
            highlight_mesh: None,
            stair_ghost_pipeline,
            stair_ghost_mesh: None,
            entity_pipeline,
            entity_mesh: None,
            skinned_pipeline,
            char_tex_layout,
            char_uniform_layout,
            char_sampler,
            character_mesh: None,
            character_instances: Vec::new(),
            character_instance_count: 0,
            viewmodel_tex_layout,
            viewmodel_pipeline,
            viewmodel: None,
            viewmodel_visible: false,
            muzzle_pipeline,
            muzzle: None,
            enemy_weapon_meshes: HashMap::new(),
            enemy_muzzle_meshes: HashMap::new(),
            enemy_weapon_clips: Vec::new(),
            enemy_muzzle_clips: Vec::new(),
            enemy_weapon_draws: Vec::new(),
            enemy_muzzle_draws: Vec::new(),
            muzzle_visible: false,
            spark_pipeline,
            spark_mesh: None,
            blast_pipeline,
            blast_atlas_bind,
            blast_mesh: None,
            door_pipeline,
            door_mesh: None,
            gizmo_pipeline,
            gizmo_mesh: None,
            crosshair_pipeline,
            overlay_buf,
            overlay_bind_group,
            crosshair_bind,
            _crosshair_tex: crosshair_tex,
            _crosshair_sampler: crosshair_sampler,
            crosshair_visible: true,
            hud_pipeline,
            hud_sampler,
            hud_atlas_bind: None,
            _hud_atlas_tex: None,
            hud_mesh: None,
            screen_pipeline,
            screen_sampler,
            _tint_layout: tint_layout,
            white_screen_bind,
            _white_screen_tex: white_screen_tex,
            health_screen_bind: None,
            _health_tex: None,
            health_dims: (0, 0),
            health_tint_buf,
            health_tint_bind,
            flash_tint_buf,
            flash_tint_bind,
            _death_tint_buf: death_tint_buf,
            death_tint_bind,
            health_visible: false,
            flash_visible: false,
            death_visible: false,
        }
    }

    /// Set (or clear) the selection-highlight quad mesh.
    pub fn set_highlight(&mut self, mesh: Option<&CpuMesh>) {
        self.highlight_mesh = match mesh {
            Some(m) if !m.indices.is_empty() => Some(GpuMesh::upload(&self.device, m)),
            _ => None,
        };
    }

    /// Set (or clear) the pending-stair ghost mesh (x-ray step preview).
    pub fn set_stair_ghost(&mut self, mesh: Option<&CpuMesh>) {
        self.stair_ghost_mesh = match mesh {
            Some(m) if !m.indices.is_empty() => Some(GpuMesh::upload(&self.device, m)),
            _ => None,
        };
    }

    /// Set (or clear) the dynamic entity mesh (the hunter). Re-uploaded each
    /// frame at its new position — cheap for a single small box.
    pub fn set_entity_mesh(&mut self, mesh: Option<&CpuMesh>) {
        self.entity_mesh = match mesh {
            Some(m) if !m.indices.is_empty() => Some(GpuMesh::upload(&self.device, m)),
            _ => None,
        };
    }

    /// Upload the shared skinned-character geometry to the GPU: shared vertex/index
    /// buffers, one GPU texture per referenced image, and per-primitive texture bind
    /// groups. Call once — all hunters (and the BUILD demo) share this mesh; each
    /// drawn instance's pose comes from [`Renderer::set_character_instances`].
    pub fn upload_character(&mut self, model: &SkinnedModel) {
        let vertex_buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("char-vertices"),
            contents: bytemuck::cast_slice(&model.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("char-indices"),
            contents: bytemuck::cast_slice(&model.indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        // Upload every referenced image to a GPU texture, plus a 1×1 white
        // fallback for primitives without a base-color texture.
        let mut textures: Vec<wgpu::Texture> = Vec::new();
        let mut views: Vec<wgpu::TextureView> = Vec::new();
        for img in &model.images {
            let tex = self.upload_char_texture(img.width, img.height, &img.rgba);
            views.push(tex.create_view(&wgpu::TextureViewDescriptor::default()));
            textures.push(tex);
        }
        let white = self.upload_char_texture(1, 1, &[255, 255, 255, 255]);
        let white_view = white.create_view(&wgpu::TextureViewDescriptor::default());
        textures.push(white);

        let primitives = model
            .primitives
            .iter()
            .map(|p| {
                let view = p.image.and_then(|i| views.get(i)).unwrap_or(&white_view);
                let tex_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("char-tex-bg"),
                    layout: &self.char_tex_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.char_sampler),
                        },
                    ],
                });
                GpuPrimitive {
                    index_start: p.index_start,
                    index_count: p.index_count,
                    tex_bind,
                }
            })
            .collect();

        self.character_mesh = Some(GpuCharacterMesh {
            vertex_buf,
            index_buf,
            primitives,
            vertex_count: model.vertices.len() as u32,
            _textures: textures,
        });
    }

    /// Build one pooled character-instance pose uniform + its bind group, plus a
    /// per-vertex blood-color buffer initialized to white (clean). Sized to the
    /// uploaded character mesh's vertex count.
    fn make_character_instance(&self) -> GpuCharacterInstance {
        let uniform_buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("char-uniform"),
            contents: bytemuck::cast_slice(&[CharUniform::default()]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let uniform_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("char-uniform-bg"),
            layout: &self.char_uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });
        let n = self.character_mesh.as_ref().map(|m| m.vertex_count).unwrap_or(0) as usize;
        let white = vec![1.0f32; n * 3];
        let color_buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("char-blood"),
            contents: bytemuck::cast_slice(&white),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });
        GpuCharacterInstance { uniform_buf, uniform_bind, color_buf }
    }

    /// Set every character instance to draw this frame as `(model, joint matrices,
    /// opacity, blood_colors)`. `blood_colors` is the flat per-vertex RGB (len =
    /// 3×vertex_count) painted by shots — white where clean. Grows the reused
    /// instance pool to fit, writes each pose uniform + blood buffer, and records
    /// the count. `joints` is truncated/padded to `MAX_JOINTS`. No-op geometry-wise
    /// if no character mesh is uploaded.
    pub fn set_character_instances(&mut self, instances: &[(Mat4, Vec<Mat4>, f32, &[f32])]) {
        self.character_instance_count = instances.len();
        while self.character_instances.len() < instances.len() {
            let inst = self.make_character_instance();
            self.character_instances.push(inst);
        }
        for (slot, (model, joints, opacity, colors)) in
            self.character_instances.iter().zip(instances)
        {
            let mut u = CharUniform {
                model: model.to_cols_array_2d(),
                opacity: [*opacity, 0.0, 0.0, 0.0],
                ..Default::default()
            };
            for (i, m) in joints.iter().take(MAX_JOINTS).enumerate() {
                u.joints[i] = m.to_cols_array_2d();
            }
            self.queue
                .write_buffer(&slot.uniform_buf, 0, bytemuck::cast_slice(&[u]));
            self.queue
                .write_buffer(&slot.color_buf, 0, bytemuck::cast_slice(colors));
        }
    }

    /// Remove the character geometry + all instances (e.g. reload).
    pub fn clear_character(&mut self) {
        self.character_mesh = None;
        self.character_instances.clear();
        self.character_instance_count = 0;
    }

    /// Build a GPU viewmodel (gun or muzzle flash) from a [`TexturedModel`]:
    /// shared vertex/index buffers, one GPU texture per referenced image (+ a
    /// 1×1 white fallback), per-primitive texture bind groups, and a clip-matrix
    /// uniform (identity until the first transform set). Shared by the gun +
    /// muzzle uploads.
    /// Build a weapon's shared GPU geometry (gun or muzzle flash): vertex/index
    /// buffers, one GPU texture per referenced image (+ white/black fallbacks), and
    /// per-primitive texture bind groups (base color + sampler + emissive). No clip
    /// uniform — see [`Renderer::make_clip`].
    fn build_weapon_mesh(&self, model: &TexturedModel, label: &str) -> GpuWeaponMesh {
        let vertex_buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(&model.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(&model.indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        let mut textures: Vec<wgpu::Texture> = Vec::new();
        let mut views: Vec<wgpu::TextureView> = Vec::new();
        for img in &model.images {
            let tex = self.upload_char_texture(img.width, img.height, &img.rgba);
            views.push(tex.create_view(&wgpu::TextureViewDescriptor::default()));
            textures.push(tex);
        }
        let white = self.upload_char_texture(1, 1, &[255, 255, 255, 255]);
        let white_view = white.create_view(&wgpu::TextureViewDescriptor::default());
        textures.push(white);
        // 1×1 black fallback for the emissive slot — primitives without an emissive
        // map (everything but the shiny-metal `*EnvMapping*` guns) add nothing.
        let black = self.upload_char_texture(1, 1, &[0, 0, 0, 255]);
        let black_view = black.create_view(&wgpu::TextureViewDescriptor::default());
        textures.push(black);

        let primitives = model
            .primitives
            .iter()
            .map(|p| {
                let view = p.image.and_then(|i| views.get(i)).unwrap_or(&white_view);
                let emissive_view = p.emissive.and_then(|i| views.get(i)).unwrap_or(&black_view);
                let tex_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("viewmodel-tex-bg"),
                    layout: &self.viewmodel_tex_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.char_sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(emissive_view),
                        },
                    ],
                });
                GpuPrimitive {
                    index_start: p.index_start,
                    index_count: p.index_count,
                    tex_bind,
                }
            })
            .collect();

        GpuWeaponMesh {
            vertex_buf,
            index_buf,
            primitives,
            _textures: textures,
        }
    }

    /// Build one pooled clip-matrix uniform (identity) + its bind group (group 0 =
    /// clip matrix, the camera layout).
    fn make_clip(&self, label: &str) -> GpuClip {
        let clip_buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(&[CameraUniform {
                view_proj: Mat4::IDENTITY.to_cols_array_2d(),
            }]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let clip_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: clip_buf.as_entire_binding(),
            }],
        });
        GpuClip { clip_buf, clip_bind }
    }

    fn build_gpu_viewmodel(&self, model: &TexturedModel, label: &str) -> GpuViewModel {
        let mesh = self.build_weapon_mesh(model, label);
        let clip = self.make_clip(label);
        GpuViewModel {
            vertex_buf: mesh.vertex_buf,
            index_buf: mesh.index_buf,
            primitives: mesh.primitives,
            clip_buf: clip.clip_buf,
            clip_bind: clip.clip_bind,
            _textures: mesh._textures,
        }
    }

    /// Upload the weapon viewmodel (the first-person gun). Call once when the
    /// weapon loads; drive the overlay transform each frame with
    /// [`Renderer::set_viewmodel_transform`].
    pub fn upload_viewmodel(&mut self, model: &TexturedModel) {
        self.viewmodel = Some(self.build_gpu_viewmodel(model, "viewmodel-gun"));
    }

    /// Upload the muzzle-flash mesh (P2). Call once; show it per frame via
    /// [`Renderer::set_muzzle_transform`] (only while a shot's flash is active).
    pub fn upload_muzzle(&mut self, model: &TexturedModel) {
        self.muzzle = Some(self.build_gpu_viewmodel(model, "muzzle-flash"));
    }

    /// Set the gun's overlay clip transform (`projection · viewmodel`) for this
    /// frame, or hide it. `Some(clip)` writes the matrix + shows the gun (HUNT);
    /// `None` hides it (BUILD). No-op if no viewmodel is uploaded.
    pub fn set_viewmodel_transform(&mut self, clip: Option<Mat4>) {
        let Some(vm) = &self.viewmodel else { return };
        match clip {
            Some(clip) => {
                self.queue.write_buffer(
                    &vm.clip_buf,
                    0,
                    bytemuck::cast_slice(&[CameraUniform {
                        view_proj: clip.to_cols_array_2d(),
                    }]),
                );
                self.viewmodel_visible = true;
            }
            None => self.viewmodel_visible = false,
        }
    }

    /// Set the muzzle-flash overlay transform for this frame, or hide it. `Some`
    /// writes the matrix + shows the flash; `None` hides it. No-op if no muzzle
    /// mesh is uploaded.
    pub fn set_muzzle_transform(&mut self, clip: Option<Mat4>) {
        let Some(m) = &self.muzzle else { return };
        match clip {
            Some(clip) => {
                self.queue.write_buffer(
                    &m.clip_buf,
                    0,
                    bytemuck::cast_slice(&[CameraUniform {
                        view_proj: clip.to_cols_array_2d(),
                    }]),
                );
                self.muzzle_visible = true;
            }
            None => self.muzzle_visible = false,
        }
    }

    /// Add one enemy weapon's gun mesh to the render library, keyed by weapon name
    /// (A3, arsenal). Call once per arsenal weapon at startup; draw any number of
    /// them per frame via [`Renderer::set_enemy_weapon_draws`].
    pub fn upload_enemy_weapon(&mut self, key: &'static str, model: &TexturedModel) {
        let mesh = self.build_weapon_mesh(model, "enemy-gun");
        self.enemy_weapon_meshes.insert(key, mesh);
    }

    /// Add one enemy weapon's muzzle-flash mesh to the render library, keyed by
    /// weapon name. Drawn per frame via [`Renderer::set_enemy_muzzle_draws`].
    pub fn upload_enemy_muzzle(&mut self, key: &'static str, model: &TexturedModel) {
        let mesh = self.build_weapon_mesh(model, "enemy-muzzle");
        self.enemy_muzzle_meshes.insert(key, mesh);
    }

    /// Set the enemy gun draws this frame: `(weapon name, view_proj · world)` per
    /// gun to render (one per hunter, two for dual-wield). Grows the reused clip
    /// pool, writes each transform, and records the draw list; the draw pass looks
    /// up each mesh by name (a draw for an unknown/failed weapon is skipped).
    pub fn set_enemy_weapon_draws(&mut self, draws: &[(&'static str, Mat4)]) {
        while self.enemy_weapon_clips.len() < draws.len() {
            let clip = self.make_clip("enemy-gun-clip");
            self.enemy_weapon_clips.push(clip);
        }
        self.enemy_weapon_draws.clear();
        for (i, (key, clip)) in draws.iter().enumerate() {
            self.queue.write_buffer(
                &self.enemy_weapon_clips[i].clip_buf,
                0,
                bytemuck::cast_slice(&[CameraUniform {
                    view_proj: clip.to_cols_array_2d(),
                }]),
            );
            self.enemy_weapon_draws.push((i, key));
        }
    }

    /// Set the enemy muzzle-flash draws this frame (same shape as
    /// [`Renderer::set_enemy_weapon_draws`]); shown only while a shot's flash is
    /// active.
    pub fn set_enemy_muzzle_draws(&mut self, draws: &[(&'static str, Mat4)]) {
        while self.enemy_muzzle_clips.len() < draws.len() {
            let clip = self.make_clip("enemy-muzzle-clip");
            self.enemy_muzzle_clips.push(clip);
        }
        self.enemy_muzzle_draws.clear();
        for (i, (key, clip)) in draws.iter().enumerate() {
            self.queue.write_buffer(
                &self.enemy_muzzle_clips[i].clip_buf,
                0,
                bytemuck::cast_slice(&[CameraUniform {
                    view_proj: clip.to_cols_array_2d(),
                }]),
            );
            self.enemy_muzzle_draws.push((i, key));
        }
    }

    /// Set (or clear) the hit-spark marker mesh (P2). Rebuilt each frame from the
    /// live spark set; `None` (or an empty mesh) clears it.
    pub fn set_spark_mesh(&mut self, mesh: Option<&ColoredMesh>) {
        self.spark_mesh = match mesh {
            Some(m) if !m.indices.is_empty() => Some(GpuMesh::upload_colored(&self.device, m)),
            _ => None,
        };
    }

    /// Set (or clear) the explosion-fireball billboard mesh (CPU-built camera-facing
    /// quads for the live blasts). `None`/empty clears it. Rebuilt each frame.
    pub fn set_blast_mesh(&mut self, mesh: Option<&TexturedMesh>) {
        self.blast_mesh = match mesh {
            Some(m) if !m.indices.is_empty() => {
                Some(GpuMesh::upload_tex(&self.device, &m.vertices, &m.indices))
            }
            _ => None,
        };
    }

    /// Remove the current weapon viewmodel + muzzle (e.g. leaving HUNT).
    pub fn clear_viewmodel(&mut self) {
        self.viewmodel = None;
        self.muzzle = None;
    }

    /// Helper: create + fill an RGBA8 sRGB GPU texture from tightly-packed pixels.
    fn upload_char_texture(&self, width: u32, height: u32, rgba: &[u8]) -> wgpu::Texture {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("char-texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            size,
        );
        tex
    }

    /// Set (or clear) the combined door-panel mesh. Re-uploaded when a door
    /// breaches (a breached panel drops out of the combined mesh); `None` clears.
    pub fn set_door_mesh(&mut self, mesh: Option<&CpuMesh>) {
        self.door_mesh = match mesh {
            Some(m) if !m.indices.is_empty() => Some(GpuMesh::upload(&self.device, m)),
            _ => None,
        };
    }

    /// Set (or clear) the platform gizmo overlay mesh. Rebuilt each frame while a
    /// platform is selected (handle colors track hover / active drag); `None` clears.
    pub fn set_gizmo_mesh(&mut self, mesh: Option<&ColoredMesh>) {
        self.gizmo_mesh = match mesh {
            Some(m) if !m.indices.is_empty() => Some(GpuMesh::upload_colored(&self.device, m)),
            _ => None,
        };
    }

    /// Upload the HUD glyph atlas once (the code-defined bitmap font as an RGBA8
    /// texture; white glyphs on a transparent background). Called at init with the
    /// game's `hud` atlas. Until this runs, HUD draws nothing.
    pub fn upload_hud_atlas(&mut self, width: u32, height: u32, rgba: &[u8]) {
        let tex = upload_rgba_srgb(&self.device, &self.queue, width, height, rgba, "hud-atlas");
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hud-atlas-bg"),
            layout: &self.char_tex_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.hud_sampler),
                },
            ],
        });
        self.hud_atlas_bind = Some(bind);
        self._hud_atlas_tex = Some(tex);
    }

    /// Set (or clear) this frame's HUD quads (screen-space NDC verts). Rebuilt each
    /// frame from the current ammo/HUD state; `None` or empty draws nothing.
    pub fn set_hud_mesh(&mut self, verts: Option<&[crate::render::mesh::HudVertex]>) {
        self.hud_mesh = match verts {
            Some(v) if !v.is_empty() => {
                let buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("hud-vertices"),
                    contents: bytemuck::cast_slice(v),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                Some((buf, v.len() as u32))
            }
            _ => None,
        };
    }

    /// Upload/replace the radial-health texture (the baked RGBA from
    /// `hud::health::HealthHud::render`). Called only when the player's health
    /// changes. Recreates the texture (health graphics are small).
    pub fn update_health_texture(&mut self, width: u32, height: u32, rgba: &[u8]) {
        let tex = upload_rgba_srgb(&self.device, &self.queue, width, height, rgba, "health-hud");
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("health-hud-bg"),
            layout: &self.char_tex_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.screen_sampler) },
            ],
        });
        self.health_screen_bind = Some(bind);
        self._health_tex = Some(tex);
        self.health_dims = (width, height);
    }

    /// Show the radial health HUD this frame at `opacity` (0 hides it). Writes the
    /// health tint's alpha.
    pub fn set_health_hud(&mut self, opacity: Option<f32>) {
        match opacity {
            Some(a) if a > 0.0 && self.health_screen_bind.is_some() => {
                self.queue.write_buffer(
                    &self.health_tint_buf,
                    0,
                    bytemuck::cast_slice(&[TintUniform { color: [1.0, 1.0, 1.0, a] }]),
                );
                self.health_visible = true;
            }
            _ => self.health_visible = false,
        }
    }

    /// Set the red damage-flash alpha this frame (0 hides it).
    pub fn set_damage_flash(&mut self, alpha: f32) {
        if alpha > 0.0 {
            self.queue.write_buffer(
                &self.flash_tint_buf,
                0,
                bytemuck::cast_slice(&[TintUniform { color: [1.0, 0.0, 0.0, alpha] }]),
            );
            self.flash_visible = true;
        } else {
            self.flash_visible = false;
        }
    }

    /// Show/hide the death dimmer (the dark full-screen overlay behind YOU DIED).
    pub fn set_death_screen(&mut self, visible: bool) {
        self.death_visible = visible;
    }

    /// Insert or replace a region's textured mesh. Called on every brush edit; an
    /// empty mesh removes the region.
    pub fn set_region_textured(&mut self, region_id: u32, mesh: &TexturedMesh) {
        if mesh.indices.is_empty() {
            self.regions.remove(&region_id);
            return;
        }
        let vertex_buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("region-tex-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("region-tex-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        self.regions.insert(
            region_id,
            TexturedRegion {
                vertex_buf,
                index_buf,
                index_count: mesh.indices.len() as u32,
                groups: mesh.groups.clone(),
            },
        );
    }

    /// Toggle checkerboard "grid" view (`true`) vs textured view (`false`).
    pub fn set_grid_mode(&mut self, grid: bool) {
        self.grid_mode = grid;
    }

    /// Whether the checkerboard grid view is active.
    pub fn is_grid_mode(&self) -> bool {
        self.grid_mode
    }

    /// Current framebuffer aspect ratio (for the camera's projection).
    pub fn aspect(&self) -> f32 {
        self.config.width as f32 / self.config.height.max(1) as f32
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.depth_view = create_depth(&self.device, width, height);
        // Keep the crosshair square after a resize (offset re-set each frame).
        self.queue.write_buffer(
            &self.overlay_buf,
            0,
            bytemuck::cast_slice(&[OverlayUniform {
                aspect_fix: height as f32 / width.max(1) as f32,
                offset_x: 0.0,
                offset_y: 0.0,
                mode: 0.0,
            }]),
        );
    }

    /// Set the free-aim reticle for this frame: `Some(offset)` shows the textured
    /// GoldenEye reticle at that screen-space NDC offset (`(0,0)` = centered);
    /// `None` hides it. Rewrites the overlay uniform (keeping the aspect
    /// correction) when shown. Used in HUNT while aiming; see
    /// [`Self::set_build_crosshair`] for the BUILD editor cursor.
    pub fn set_crosshair_offset(&mut self, offset: Option<(f32, f32)>) {
        match offset {
            Some((ox, oy)) => {
                self.crosshair_visible = true;
                let aspect_fix = self.config.height as f32 / self.config.width.max(1) as f32;
                self.queue.write_buffer(
                    &self.overlay_buf,
                    0,
                    bytemuck::cast_slice(&[OverlayUniform {
                        aspect_fix,
                        offset_x: ox,
                        offset_y: oy,
                        mode: 0.0,
                    }]),
                );
            }
            None => self.crosshair_visible = false,
        }
    }

    /// Show the small white BUILD-mode cross, centered — the editor's pick cursor
    /// (a procedural cross in the shader, no texture). Distinct from the HUNT
    /// free-aim reticle ([`Self::set_crosshair_offset`]); the caller shows one or
    /// the other per frame.
    pub fn set_build_crosshair(&mut self) {
        self.crosshair_visible = true;
        let aspect_fix = self.config.height as f32 / self.config.width.max(1) as f32;
        self.queue.write_buffer(
            &self.overlay_buf,
            0,
            bytemuck::cast_slice(&[OverlayUniform {
                aspect_fix,
                offset_x: 0.0,
                offset_y: 0.0,
                mode: 1.0,
            }]),
        );
    }

    pub fn render(&mut self, view_proj: Mat4) {
        self.queue.write_buffer(
            &self.camera_buf,
            0,
            bytemuck::cast_slice(&[CameraUniform {
                view_proj: view_proj.to_cols_array_2d(),
            }]),
        );

        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(_) => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };
        let view_tex = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("forward-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view_tex,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.02,
                            g: 0.02,
                            b: 0.05,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            // 1) Opaque region meshes — grid (checkerboard) or textured view.
            rp.set_bind_group(0, &self.camera_bind_group, &[]);
            if self.grid_mode {
                rp.set_pipeline(&self.pipeline);
                for m in self.regions.values() {
                    rp.set_vertex_buffer(0, m.vertex_buf.slice(..));
                    rp.set_index_buffer(m.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    rp.draw_indexed(0..m.index_count, 0, 0..1);
                }
            } else {
                rp.set_pipeline(&self.textured_pipeline);
                for m in self.regions.values() {
                    rp.set_vertex_buffer(0, m.vertex_buf.slice(..));
                    rp.set_index_buffer(m.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    for g in &m.groups {
                        // Bind the (scheme, zone) material for this group; skip the
                        // (rare) undefined zone rather than draw untextured.
                        if let Some(bg) = self
                            .materials
                            .get(g.scheme as usize)
                            .and_then(|z| z[g.zone as usize].as_ref())
                        {
                            rp.set_bind_group(1, bg, &[]);
                            rp.draw_indexed(g.start..(g.start + g.count), 0, 0..1);
                        }
                    }
                }
            }

            // 2) Dynamic entities (opaque, before the translucent highlight).
            if let Some(e) = &self.entity_mesh {
                rp.set_pipeline(&self.entity_pipeline);
                rp.set_bind_group(0, &self.camera_bind_group, &[]);
                rp.set_vertex_buffer(0, e.vertex_buf.slice(..));
                rp.set_index_buffer(e.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                rp.draw_indexed(0..e.index_count, 0, 0..1);
            }

            // 2.1) Hit sparks (opaque, depth-tested, bright unlit markers).
            if let Some(s) = &self.spark_mesh {
                rp.set_pipeline(&self.spark_pipeline);
                rp.set_bind_group(0, &self.camera_bind_group, &[]);
                rp.set_vertex_buffer(0, s.vertex_buf.slice(..));
                rp.set_index_buffer(s.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                rp.draw_indexed(0..s.index_count, 0, 0..1);
            }

            // 2.2) Skinned characters (opaque, unlit textured) — one draw per live
            // hunter (or the BUILD demo). group(0)=camera; group(2)=this instance's
            // joints/model; group(1)=texture per primitive. All share one mesh.
            if let Some(ch) = &self.character_mesh {
                rp.set_pipeline(&self.skinned_pipeline);
                rp.set_bind_group(0, &self.camera_bind_group, &[]);
                rp.set_vertex_buffer(0, ch.vertex_buf.slice(..));
                rp.set_index_buffer(ch.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                for inst in self.character_instances.iter().take(self.character_instance_count) {
                    rp.set_bind_group(2, &inst.uniform_bind, &[]);
                    // Per-instance blood colors in the second vertex buffer.
                    rp.set_vertex_buffer(1, inst.color_buf.slice(..));
                    for p in &ch.primitives {
                        rp.set_bind_group(1, &p.tex_bind, &[]);
                        rp.draw_indexed(p.index_start..(p.index_start + p.index_count), 0, 0..1);
                    }
                }
            }

            // 2.3) Enemy guns attached to the hunters' hand bones (world-space,
            // depth-tested vs the scene — reuses the viewmodel pipeline with a
            // view_proj·world clip matrix). One draw per gun (two for dual-wield),
            // each looking up its mesh by weapon name.
            for (clip_idx, key) in &self.enemy_weapon_draws {
                let (Some(w), Some(clip)) = (
                    self.enemy_weapon_meshes.get(key),
                    self.enemy_weapon_clips.get(*clip_idx),
                ) else {
                    continue;
                };
                rp.set_pipeline(&self.viewmodel_pipeline);
                rp.set_bind_group(0, &clip.clip_bind, &[]);
                rp.set_vertex_buffer(0, w.vertex_buf.slice(..));
                rp.set_index_buffer(w.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                for p in &w.primitives {
                    rp.set_bind_group(1, &p.tex_bind, &[]);
                    rp.draw_indexed(p.index_start..(p.index_start + p.index_count), 0, 0..1);
                }
            }
            // 2.4) Enemy muzzle flashes (additive) while shots are firing.
            for (clip_idx, key) in &self.enemy_muzzle_draws {
                let (Some(m), Some(clip)) = (
                    self.enemy_muzzle_meshes.get(key),
                    self.enemy_muzzle_clips.get(*clip_idx),
                ) else {
                    continue;
                };
                rp.set_pipeline(&self.muzzle_pipeline);
                rp.set_bind_group(0, &clip.clip_bind, &[]);
                rp.set_vertex_buffer(0, m.vertex_buf.slice(..));
                rp.set_index_buffer(m.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                for p in &m.primitives {
                    rp.set_bind_group(1, &p.tex_bind, &[]);
                    rp.draw_indexed(p.index_start..(p.index_start + p.index_count), 0, 0..1);
                }
            }

            // 2.45) Explosion fireballs (additive camera-facing billboards). After the
            // opaque scene so depth-test occludes them behind nearer walls; additive +
            // no depth-write so overlapping quads glow. One mesh for all live blasts.
            if let Some(b) = &self.blast_mesh {
                rp.set_pipeline(&self.blast_pipeline);
                rp.set_bind_group(0, &self.camera_bind_group, &[]);
                rp.set_bind_group(1, &self.blast_atlas_bind, &[]);
                rp.set_vertex_buffer(0, b.vertex_buf.slice(..));
                rp.set_index_buffer(b.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                rp.draw_indexed(0..b.index_count, 0, 0..1);
            }

            // 2.5) Breakable door panels (opaque brown).
            if let Some(dm) = &self.door_mesh {
                rp.set_pipeline(&self.door_pipeline);
                rp.set_bind_group(0, &self.camera_bind_group, &[]);
                rp.set_vertex_buffer(0, dm.vertex_buf.slice(..));
                rp.set_index_buffer(dm.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                rp.draw_indexed(0..dm.index_count, 0, 0..1);
            }

            // 3) Selection highlight (translucent, over the picked face).
            if let Some(h) = &self.highlight_mesh {
                rp.set_pipeline(&self.highlight_pipeline);
                rp.set_bind_group(0, &self.camera_bind_group, &[]);
                rp.set_vertex_buffer(0, h.vertex_buf.slice(..));
                rp.set_index_buffer(h.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                rp.draw_indexed(0..h.index_count, 0, 0..1);
            }

            // 3.5) Pending-stair ghost (translucent, x-ray through the wall).
            if let Some(g) = &self.stair_ghost_mesh {
                rp.set_pipeline(&self.stair_ghost_pipeline);
                rp.set_bind_group(0, &self.camera_bind_group, &[]);
                rp.set_vertex_buffer(0, g.vertex_buf.slice(..));
                rp.set_index_buffer(g.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                rp.draw_indexed(0..g.index_count, 0, 0..1);
            }

            // 3.6) Platform gizmo handles (always-on-top, unlit colored).
            if let Some(g) = &self.gizmo_mesh {
                rp.set_pipeline(&self.gizmo_pipeline);
                rp.set_bind_group(0, &self.camera_bind_group, &[]);
                rp.set_vertex_buffer(0, g.vertex_buf.slice(..));
                rp.set_index_buffer(g.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                rp.draw_indexed(0..g.index_count, 0, 0..1);
            }

        } // end forward pass

        // ── Overlay pass: depth is CLEARED here so the first-person weapon
        // viewmodel is always on top and never clips into world geometry (exactly
        // like a real FPS view weapon). Color is loaded (the world stays). The gun
        // draws first, then the screen-space crosshair on top of everything.
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("overlay-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view_tex,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Weapon viewmodel (the gun): group(0)=clip matrix, group(1)=texture
            // per primitive. Uploaded once, shown only in HUNT (per-frame flag).
            if let (Some(vm), true) = (&self.viewmodel, self.viewmodel_visible) {
                rp.set_pipeline(&self.viewmodel_pipeline);
                rp.set_bind_group(0, &vm.clip_bind, &[]);
                rp.set_vertex_buffer(0, vm.vertex_buf.slice(..));
                rp.set_index_buffer(vm.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                for p in &vm.primitives {
                    rp.set_bind_group(1, &p.tex_bind, &[]);
                    rp.draw_indexed(p.index_start..(p.index_start + p.index_count), 0, 0..1);
                }
            }

            // Muzzle flash on top of the gun (additive), only while active.
            if let (Some(m), true) = (&self.muzzle, self.muzzle_visible) {
                rp.set_pipeline(&self.muzzle_pipeline);
                rp.set_bind_group(0, &m.clip_bind, &[]);
                rp.set_vertex_buffer(0, m.vertex_buf.slice(..));
                rp.set_index_buffer(m.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                for p in &m.primitives {
                    rp.set_bind_group(1, &p.tex_bind, &[]);
                    rp.draw_indexed(p.index_start..(p.index_start + p.index_count), 0, 0..1);
                }
            }

            // Screen-space crosshair (textured, alpha-blended, no depth).
            // Shown only while aiming (HUNT) or in BUILD (editor pick cursor).
            if self.crosshair_visible {
                rp.set_pipeline(&self.crosshair_pipeline);
                rp.set_bind_group(0, &self.overlay_bind_group, &[]);
                rp.set_bind_group(1, &self.crosshair_bind, &[]);
                rp.draw(0..6, 0..1);
            }

            // Full-screen overlays (P5), painter-ordered like the JS z-indices:
            // red damage flash (19) → radial health HUD (20) → death dimmer (30).
            if self.flash_visible {
                rp.set_pipeline(&self.screen_pipeline);
                rp.set_bind_group(0, &self.white_screen_bind, &[]);
                rp.set_bind_group(1, &self.flash_tint_bind, &[]);
                rp.draw(0..6, 0..1);
            }
            if let (true, Some(health)) = (self.health_visible, &self.health_screen_bind) {
                rp.set_pipeline(&self.screen_pipeline);
                rp.set_bind_group(0, health, &[]);
                rp.set_bind_group(1, &self.health_tint_bind, &[]);
                rp.draw(0..6, 0..1);
            }
            if self.death_visible {
                rp.set_pipeline(&self.screen_pipeline);
                rp.set_bind_group(0, &self.white_screen_bind, &[]);
                rp.set_bind_group(1, &self.death_tint_bind, &[]);
                rp.draw(0..6, 0..1);
            }

            // HUD text (ammo counter, or YOU DIED / PRESS R), last — on top.
            if let (Some(bind), Some((buf, count))) = (&self.hud_atlas_bind, &self.hud_mesh) {
                rp.set_pipeline(&self.hud_pipeline);
                rp.set_bind_group(0, bind, &[]);
                rp.set_vertex_buffer(0, buf.slice(..));
                rp.draw(0..*count, 0..1);
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
    }
}

/// Graphics backend, overridable via `BH_BACKEND=dx12|vulkan|gl` so we can A/B
/// the presentation path at runtime. Default Vulkan (Phase 0's locked choice).
/// DX12 is the flip-model path a browser uses on Windows — useful for latency
/// comparisons.
fn pick_backends() -> wgpu::Backends {
    match std::env::var("BH_BACKEND").unwrap_or_default().to_lowercase().as_str() {
        "dx12" | "d3d12" => wgpu::Backends::DX12,
        "gl" | "opengl" => wgpu::Backends::GL,
        "vulkan" | "vk" | "" => wgpu::Backends::VULKAN,
        other => {
            log::warn!("unknown BH_BACKEND={other:?}; using Vulkan");
            wgpu::Backends::VULKAN
        }
    }
}

/// Present mode, overridable via `BH_PRESENT=mailbox|immediate|fifo`. Default
/// prefers Mailbox (present newest frame, no vsync wait — lowest latency) and
/// falls back to Fifo where it isn't supported.
fn pick_present_mode(available: &[wgpu::PresentMode]) -> wgpu::PresentMode {
    use wgpu::PresentMode::*;
    let pref: &[wgpu::PresentMode] =
        match std::env::var("BH_PRESENT").unwrap_or_default().to_lowercase().as_str() {
            "fifo" | "vsync" => &[Fifo],
            "immediate" | "novsync" => &[Immediate, Mailbox, Fifo],
            "mailbox" => &[Mailbox, Immediate, Fifo],
            _ => &[Mailbox, Fifo], // default: low-latency where possible
        };
    pref.iter()
        .copied()
        .find(|p| available.contains(p))
        .unwrap_or(wgpu::PresentMode::Fifo)
}

/// Decode every scheme's textures (deduped by name) into GPU textures and build
/// the `materials[scheme][zone]` bind-group table. Returns the table plus the
/// textures and uniform buffers that must be kept alive for the bind groups.
fn build_materials(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
) -> (Vec<[Option<wgpu::BindGroup>; 8]>, Vec<wgpu::Texture>, Vec<wgpu::Buffer>) {
    let mut keepalive: Vec<wgpu::Texture> = Vec::new();
    let mut buffers: Vec<wgpu::Buffer> = Vec::new();
    let mut view_by_name: HashMap<&'static str, wgpu::TextureView> = HashMap::new();

    let mut materials: Vec<[Option<wgpu::BindGroup>; 8]> = Vec::new();
    for scheme in textures::SCHEMES {
        let mut zones: [Option<wgpu::BindGroup>; 8] = std::array::from_fn(|_| None);
        for (zi, zone) in scheme.zones.iter().enumerate() {
            let Some(zdef) = zone else { continue };
            let Some(name) = zdef.texture else { continue };

            if !view_by_name.contains_key(name) {
                let Some(dec) = textures::decode(name) else {
                    log::warn!("texture {name} failed to decode; zone left untextured");
                    continue;
                };
                let size = wgpu::Extent3d {
                    width: dec.width,
                    height: dec.height,
                    depth_or_array_layers: 1,
                };
                let tex = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(name),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    // sRGB: the BMPs are authored in gamma space and the surface
                    // is sRGB, so decode-on-sample + encode-on-write is correct.
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &dec.rgba,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4 * dec.width),
                        rows_per_image: Some(dec.height),
                    },
                    size,
                );
                view_by_name.insert(name, tex.create_view(&wgpu::TextureViewDescriptor::default()));
                keepalive.push(tex);
            }

            let Some(view) = view_by_name.get(name) else { continue };
            let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("material-uniform"),
                contents: bytemuck::cast_slice(&[MaterialUniform {
                    params: [zdef.repeat, 0.0, 0.0, 0.0],
                }]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("material-bg"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: ubuf.as_entire_binding(),
                    },
                ],
            });
            buffers.push(ubuf);
            zones[zi] = Some(bg);
        }
        materials.push(zones);
    }
    (materials, keepalive, buffers)
}

/// Load the crosshair reticle PNG (`assets/hud/crosshairs.png`) as RGBA8 from the
/// runtime asset dir. On any failure, warn + return a magenta 2×2 so the miss is
/// obvious on screen rather than an invisible crosshair.
/// Load the baked GoldenEye explosion fireball atlas (8 pre-coloured frames laid
/// out horizontally, 448×56 RGBA). A magenta fallback makes a missing file obvious.
fn load_explosion_atlas_rgba() -> (u32, u32, Vec<u8>) {
    let path = format!("{}/../../assets/vfx/explosion_atlas.png", env!("CARGO_MANIFEST_DIR"));
    match image::open(&path) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            log::info!("loaded explosion atlas {w}×{h}");
            (w, h, rgba.into_raw())
        }
        Err(e) => {
            log::warn!("explosion atlas load failed ({path}): {e}");
            (2, 2, vec![255, 0, 255, 255].repeat(4))
        }
    }
}

fn load_crosshair_rgba() -> (u32, u32, Vec<u8>) {
    let path = format!("{}/../../assets/hud/crosshairs.png", env!("CARGO_MANIFEST_DIR"));
    match image::open(&path) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            (w, h, rgba.into_raw())
        }
        Err(e) => {
            log::warn!("crosshair load failed ({path}): {e}");
            (2, 2, vec![255, 0, 255, 255].repeat(4))
        }
    }
}

/// Create + fill an RGBA8 sRGB GPU texture from tightly-packed pixels (used for
/// the crosshair; the character path has its own on `Renderer`).
fn upload_rgba_srgb(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    rgba: &[u8],
    label: &str,
) -> wgpu::Texture {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * width),
            rows_per_image: Some(height),
        },
        size,
    );
    tex
}

fn create_depth(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}
