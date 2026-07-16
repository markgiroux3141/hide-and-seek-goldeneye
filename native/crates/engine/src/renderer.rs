//! wgpu renderer (Vulkan backend). Phase 1 scope: one forward pipeline with a
//! depth buffer and a single camera uniform, drawing per-region meshes that can
//! be replaced live as brushes are edited. The camera is external (a
//! [`crate::camera::FlyCamera`]); the renderer just consumes a view-proj matrix.

use std::collections::HashMap;
use std::sync::Arc;

use glam::Mat4;
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::combat::GunModel;
use crate::mesh::{
    ColorVertex, ColoredMesh, CpuMesh, GpuMesh, SkinVertex, TexVertex, TexturedMesh, Vertex,
    ZoneGroup,
};
use crate::skeletal::gltf_skin::SkinnedModel;
use crate::textures;

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
}

impl Default for CharUniform {
    fn default() -> Self {
        CharUniform {
            model: Mat4::IDENTITY.to_cols_array_2d(),
            joints: [Mat4::IDENTITY.to_cols_array_2d(); MAX_JOINTS],
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

/// A GPU-resident skinned character: shared vertex/index buffers, per-texture
/// primitives, and the per-character joint/model uniform (rewritten each frame
/// as the pose animates).
struct GpuCharacter {
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    primitives: Vec<GpuPrimitive>,
    uniform_buf: wgpu::Buffer,
    uniform_bind: wgpu::BindGroup,
    _textures: Vec<wgpu::Texture>,
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
    character: Option<GpuCharacter>,

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

    // Hit sparks (Player Combat P2): bright per-vertex-colored markers at shot
    // impact points. Reuses the gizmo shader (unlit color) but depth-TESTED (so
    // sparks are occluded by geometry, unlike the always-on-top gizmo). Rebuilt
    // each frame from the live spark set.
    spark_pipeline: wgpu::RenderPipeline,
    spark_mesh: Option<GpuMesh>,

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
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct OverlayUniform {
    aspect_fix: f32,
    offset_x: f32,
    offset_y: f32,
    _pad: f32,
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
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
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
            source: wgpu::ShaderSource::Wgsl(include_str!("shader_textured.wgsl").into()),
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
            source: wgpu::ShaderSource::Wgsl(include_str!("shader_highlight.wgsl").into()),
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
            source: wgpu::ShaderSource::Wgsl(include_str!("shader_entity.wgsl").into()),
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
        let char_uniform_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("char-uniform-bgl"),
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
            source: wgpu::ShaderSource::Wgsl(include_str!("shader_skinned.wgsl").into()),
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
                buffers: &[SkinVertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &skinned_shader,
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
            source: wgpu::ShaderSource::Wgsl(include_str!("shader_viewmodel.wgsl").into()),
        });
        let viewmodel_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewmodel-layout"),
            bind_group_layouts: &[&camera_layout, &char_tex_layout],
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
            source: wgpu::ShaderSource::Wgsl(include_str!("shader_door.wgsl").into()),
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
            source: wgpu::ShaderSource::Wgsl(include_str!("gizmo.wgsl").into()),
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
        // viewmodel, but ADDITIVE blend + no depth write — a flash of light on top
        // of the gun (JS `AdditiveBlending`, `depthWrite=false`, `DoubleSide`).
        // Drawn in the overlay pass (depth already cleared), always over the gun.
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
                depth_compare: wgpu::CompareFunction::Always,
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

        // ── Crosshair pipeline: screen-space `+`, no camera, no depth test.
        let overlay_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("overlay-uniform"),
            contents: bytemuck::cast_slice(&[OverlayUniform {
                aspect_fix: config.height as f32 / config.width.max(1) as f32,
                offset_x: 0.0,
                offset_y: 0.0,
                _pad: 0.0,
            }]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let overlay_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay-bgl"),
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
            source: wgpu::ShaderSource::Wgsl(include_str!("shader_crosshair.wgsl").into()),
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
            character: None,
            viewmodel_pipeline,
            viewmodel: None,
            viewmodel_visible: false,
            muzzle_pipeline,
            muzzle: None,
            muzzle_visible: false,
            spark_pipeline,
            spark_mesh: None,
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

    /// Upload a skinned character to the GPU: shared vertex/index buffers, one
    /// GPU texture per referenced image, per-primitive texture bind groups, and
    /// the per-character joint/model uniform (initialized to the bind pose).
    /// Call once per spawned character; drive the pose each frame with
    /// [`Renderer::set_character_pose`].
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

        self.character = Some(GpuCharacter {
            vertex_buf,
            index_buf,
            primitives,
            uniform_buf,
            uniform_bind,
            _textures: textures,
        });
    }

    /// Update the current character's world placement + joint matrices (called
    /// each frame). `joints` is truncated/padded to `MAX_JOINTS` with identity.
    /// No-op if no character is uploaded.
    pub fn set_character_pose(&mut self, model: Mat4, joints: &[Mat4]) {
        let Some(ch) = &self.character else { return };
        let mut u = CharUniform {
            model: model.to_cols_array_2d(),
            ..Default::default()
        };
        for (i, m) in joints.iter().take(MAX_JOINTS).enumerate() {
            u.joints[i] = m.to_cols_array_2d();
        }
        self.queue
            .write_buffer(&ch.uniform_buf, 0, bytemuck::cast_slice(&[u]));
    }

    /// Remove the current character.
    pub fn clear_character(&mut self) {
        self.character = None;
    }

    /// Build a GPU viewmodel (gun or muzzle flash) from a [`GunModel`]: shared
    /// vertex/index buffers, one GPU texture per referenced image (+ a 1×1 white
    /// fallback), per-primitive texture bind groups, and a clip-matrix uniform
    /// (identity until the first transform set). Shared by the gun + muzzle uploads.
    fn build_gpu_viewmodel(&self, model: &GunModel, label: &str) -> GpuViewModel {
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

        let primitives = model
            .primitives
            .iter()
            .map(|p| {
                let view = p.image.and_then(|i| views.get(i)).unwrap_or(&white_view);
                let tex_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("viewmodel-tex-bg"),
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

        let clip_buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("viewmodel-clip"),
            contents: bytemuck::cast_slice(&[CameraUniform {
                view_proj: Mat4::IDENTITY.to_cols_array_2d(),
            }]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let clip_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("viewmodel-clip-bg"),
            layout: &self.camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: clip_buf.as_entire_binding(),
            }],
        });

        GpuViewModel {
            vertex_buf,
            index_buf,
            primitives,
            clip_buf,
            clip_bind,
            _textures: textures,
        }
    }

    /// Upload the weapon viewmodel (the first-person gun). Call once when the
    /// weapon loads; drive the overlay transform each frame with
    /// [`Renderer::set_viewmodel_transform`].
    pub fn upload_viewmodel(&mut self, model: &GunModel) {
        self.viewmodel = Some(self.build_gpu_viewmodel(model, "viewmodel-gun"));
    }

    /// Upload the muzzle-flash mesh (P2). Call once; show it per frame via
    /// [`Renderer::set_muzzle_transform`] (only while a shot's flash is active).
    pub fn upload_muzzle(&mut self, model: &GunModel) {
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

    /// Set (or clear) the hit-spark marker mesh (P2). Rebuilt each frame from the
    /// live spark set; `None` (or an empty mesh) clears it.
    pub fn set_spark_mesh(&mut self, mesh: Option<&ColoredMesh>) {
        self.spark_mesh = match mesh {
            Some(m) if !m.indices.is_empty() => Some(GpuMesh::upload_colored(&self.device, m)),
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
                _pad: 0.0,
            }]),
        );
    }

    /// Set the crosshair for this frame: `Some(offset)` shows it at that
    /// screen-space NDC offset (GoldenEye free-aim floats it; `(0,0)` = centered);
    /// `None` hides it entirely. Rewrites the overlay uniform (keeping the aspect
    /// correction) when shown.
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
                        _pad: 0.0,
                    }]),
                );
            }
            None => self.crosshair_visible = false,
        }
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

            // 2.2) Skinned character (opaque, unlit textured). group(0)=camera,
            // group(2)=joints/model set once; group(1)=texture per primitive.
            if let Some(ch) = &self.character {
                rp.set_pipeline(&self.skinned_pipeline);
                rp.set_bind_group(0, &self.camera_bind_group, &[]);
                rp.set_bind_group(2, &ch.uniform_bind, &[]);
                rp.set_vertex_buffer(0, ch.vertex_buf.slice(..));
                rp.set_index_buffer(ch.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                for p in &ch.primitives {
                    rp.set_bind_group(1, &p.tex_bind, &[]);
                    rp.draw_indexed(p.index_start..(p.index_start + p.index_count), 0, 0..1);
                }
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

            // Screen-space crosshair, last (textured, alpha-blended, no depth).
            // Shown only while aiming (HUNT) or in BUILD (editor pick cursor).
            if self.crosshair_visible {
                rp.set_pipeline(&self.crosshair_pipeline);
                rp.set_bind_group(0, &self.overlay_bind_group, &[]);
                rp.set_bind_group(1, &self.crosshair_bind, &[]);
                rp.draw(0..6, 0..1);
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
