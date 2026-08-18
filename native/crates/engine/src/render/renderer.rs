//! wgpu renderer (Vulkan backend). Phase 1 scope: one forward pipeline with a
//! depth buffer and a single camera uniform, drawing per-region meshes that can
//! be replaced live as brushes are edited. The camera is external (a
//! [`crate::render::camera::FlyCamera`]); the renderer just consumes a view-proj matrix.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat4, Vec3};
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

/// Edge length (texels) of the square offscreen texture the shop's rotating weapon
/// preview renders into. Sampled by egui as an image in the shop panel.
const PREVIEW_SIZE: u32 = 320;

/// Max joints in the skinned-character uniform. The GoldenEye skeleton is 15
/// bones, but a **Perfect Dark** body declares 30 joints — `Bone_1..15` plus
/// `Blend_1..15`, PD's midpoint frames, exported as real joints. 32 covers both
/// with headroom and keeps the array 16-aligned. Must match
/// `shader_skinned.wgsl`'s `MAX_JOINTS`.
///
/// This was 16, which silently truncated every PD blend joint: the CPU wrote only
/// the first 16 matrices and the shader clamped the out-of-range indices onto
/// joint 15, so PD bodies drew as a fan of stretched triangles instead of a
/// person. Nothing headless caught it — `skinning_matrices` returned all 30,
/// finite and correct, and `pd_preview.py` skins on the CPU with no such cap. It
/// only exists on the GPU path.
const MAX_JOINTS: usize = 32;

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

/// Max simultaneous point lights fed to the lit region shaders. A plain uniform
/// array the fragment shader loops over — cheap, and 32 lights is ~1.5 KB, far under
/// the uniform-buffer size limit.
const MAX_LIGHTS: usize = 32;

/// Max lights that cast (omnidirectional cube) shadows at once — the rest still light
/// but without shadows. Each caster costs 6 depth-ish faces/frame, so this is the
/// dominant shadow-perf knob. The shadow cube-array holds `MAX_SHADOW_LIGHTS * 6`
/// layers.
const MAX_SHADOW_LIGHTS: usize = 4;

/// Per-face resolution of each shadow cube (square). 512 balances crispness vs the
/// 24-face-per-frame fill cost.
const SHADOW_SIZE: u32 = 512;

/// R32F distance stored in the shadow cubes (linear light-distance / range).
const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;

/// One point light on the GPU. `pos_range.xyz` = world position (metres),
/// `pos_range.w` = falloff radius (metres); `color_intensity.rgb` = linear colour,
/// `color_intensity.w` = intensity; `params.x` = shadow cube index (0..MAX_SHADOW_LIGHTS)
/// or a negative value for a non-caster. Three vec4s = 48 bytes → std140-clean.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuLight {
    pos_range: [f32; 4],
    color_intensity: [f32; 4],
    params: [f32; 4],
}

/// Per-face uniform for the shadow pass: the light-face view-proj plus the light
/// position + range (`light_pos.w`) so the fragment can store its normalised
/// distance. Matches `Face` in `shader_shadow.wgsl`.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct FaceUniform {
    view_proj: [[f32; 4]; 4],
    light_pos: [f32; 4],
}

/// Scene lighting uniform shared by the lit region shaders. `ambient.rgb` =
/// premultiplied ambient (colour × level); `ambient.w` = the flat-lighting flag
/// (1 = legacy fixed-directional look, 0 = real point lights). `count.x` = active
/// light count. Matches `Lighting` in `shader.wgsl` / `shader_textured.wgsl`.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct LightingUniform {
    ambient: [f32; 4],
    count: [u32; 4],
    lights: [GpuLight; MAX_LIGHTS],
}

impl Default for LightingUniform {
    fn default() -> Self {
        LightingUniform {
            // Flat (w = 1) by default: identical to the pre-lighting look until the
            // app pushes real lighting.
            ambient: [0.15, 0.15, 0.15, 1.0],
            count: [0; 4],
            lights: [GpuLight {
                pos_range: [0.0; 4],
                color_intensity: [0.0; 4],
                params: [-1.0, 0.0, 0.0, 0.0], // shadow index -1 = no shadow
            }; MAX_LIGHTS],
        }
    }
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

/// A GPU-resident skinned character body's geometry: vertex/index buffers +
/// per-texture primitives + the decoded textures. One is uploaded per body id (see
/// [`Renderer::character_meshes`]); each hunter selects its body and its per-instance
/// pose lives in [`GpuCharacterInstance`].
struct GpuCharacterMesh {
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    /// How many vertices `vertex_buf` holds — the size a drawing instance's
    /// blood-color buffer must match, including for a caller that supplies no
    /// blood at all (see [`Renderer::set_character_instances`]).
    vertex_count: u32,
    primitives: Vec<GpuPrimitive>,
    _textures: Vec<wgpu::Texture>,
}

/// One drawn instance of a character mesh: which body it draws + its own
/// joint/model/opacity uniform + its own per-vertex blood-color buffer (all
/// rewritten each frame). Pooled + reused across frames so N hunters draw with
/// distinct poses and independent accumulated blood. A pooled slot can be reused for
/// a different body between frames, so the blood buffer is re-sized whenever the
/// body's vertex count changes (bodies differ in geometry).
struct GpuCharacterInstance {
    /// The body id this slot draws this frame — an index into
    /// [`Renderer::character_meshes`].
    body: usize,
    uniform_buf: wgpu::Buffer,
    uniform_bind: wgpu::BindGroup,
    /// Per-vertex RGB damage/blood color (second vertex buffer). White = clean.
    color_buf: wgpu::Buffer,
    /// Vertex count the `color_buf` is currently sized for (to detect a body switch).
    color_verts: u32,
    /// Whether `color_buf` currently holds all-white (no blood painted into it).
    /// Lets a slot that draws an unpainted body skip the per-frame upload, while
    /// still being scrubbed clean when it takes over from a bloodied hunter.
    color_clean: bool,
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
    /// Model-space bounding-sphere centre + radius (from the source vertices), used
    /// to frame the gun in the shop's turntable preview regardless of its native
    /// GoldenEye-units scale (the guns are ~1000× metres). Unused by the in-game draw.
    center: Vec3,
    radius: f32,
}

/// A pooled clip-matrix uniform (`view_proj · world`) + its bind group, reused
/// frame-to-frame so a variable number of enemy weapon draws each get their own
/// transform without reallocating buffers.
struct GpuClip {
    clip_buf: wgpu::Buffer,
    clip_bind: wgpu::BindGroup,
}

/// Per-draw uniform for a placed prop: the clip matrix (`view_proj · world`) plus a
/// per-instance `tint` (rgba, multiplied over the texel — white = untouched). The
/// tint carries the Milestone-3 "darken when shot" darkening; it is always white in
/// Milestone 1. Its own layout (vs [`GpuClip`]'s) because the fragment stage reads
/// the tint, so the uniform must be VERTEX+FRAGMENT visible.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct PropUniform {
    view_proj: [[f32; 4]; 4],
    /// Model→world (metres), so the fragment stage can light the prop in world space.
    world: [[f32; 4]; 4],
    tint: [f32; 4],
}

/// A pooled [`PropUniform`] buffer + its bind group, reused frame-to-frame so a
/// variable number of prop draws each get their own transform + tint without
/// reallocating (mirrors [`GpuClip`] pooling for the enemy-weapon channel).
struct GpuPropSlot {
    buf: wgpu::Buffer,
    bind: wgpu::BindGroup,
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
    /// Allocated capacity (bytes) of each buffer, so a re-bake whose mesh fits can
    /// `write_buffer` in place instead of reallocating (the BUILD hot path).
    vertex_cap: u64,
    index_cap: u64,
    index_count: u32,
    groups: Vec<ZoneGroup>,
}

/// One frame of tessellated egui output, produced by the game app (which owns the
/// egui `Context` + `egui_winit::State`) and handed to [`Renderer::render`] to paint
/// over the game. Bundling it here keeps egui's platform/UI half in the app and the
/// GPU/painter half in the engine.
pub struct EguiFrame {
    /// Textures egui created / updated / freed this frame (font atlas, images).
    pub textures_delta: egui::TexturesDelta,
    /// The clipped triangle meshes to draw, already tessellated at `pixels_per_point`.
    pub paint_jobs: Vec<egui::ClippedPrimitive>,
    /// Points → physical-pixels scale (DPI); drives the egui screen descriptor.
    pub pixels_per_point: f32,
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
    /// Scene lighting uniform (point lights + ambient + flat/real flag) shared by
    /// the lit region shaders (grid `pipeline` at group(1), `textured_pipeline` at
    /// group(2)). Rewritten each frame from the app via [`Renderer::set_lighting`].
    lighting_buf: wgpu::Buffer,
    /// Also binds the shadow cube-array (binding 1) + its sampler (binding 2).
    lighting_bind_group: wgpu::BindGroup,
    /// Always-flat lighting group bound only by the prop-preview turntable.
    preview_lighting_bind_group: wgpu::BindGroup,

    // ── Omnidirectional shadow maps: one distance cube per shadow-casting light,
    // packed as a single R32F cube-array (`MAX_SHADOW_LIGHTS` cubes = ×6 layers).
    /// The cube-array texture (kept alive for its views).
    _shadow_cube: wgpu::Texture,
    /// One 2D render view per (light, face) — `MAX_SHADOW_LIGHTS * 6` of them, the
    /// colour targets the shadow pass renders each face into.
    shadow_face_views: Vec<wgpu::TextureView>,
    /// Shared depth buffer for the shadow render (cleared per face).
    shadow_depth_view: wgpu::TextureView,
    /// Depth+R32F pipeline that stores light-distance with cutout discard.
    shadow_pipeline: wgpu::RenderPipeline,
    /// Pooled per-face uniforms (view-proj + light pos/range), one per face view.
    shadow_face_slots: Vec<(wgpu::Buffer, wgpu::BindGroup)>,
    /// This frame's shadow casters `(world_pos_m, range_m)`, in shadow-cube-index
    /// order (≤ `MAX_SHADOW_LIGHTS`). Set by the app via [`Renderer::set_shadow_casters`].
    shadow_casters: Vec<(Vec3, f32)>,
    depth_view: wgpu::TextureView,

    /// egui painter (menus: the shop + inventory panels). The game app owns the
    /// egui `Context` + winit event translation and hands us its tessellated output
    /// each frame via [`EguiFrame`]; this paints it in a final overlay pass. `None`
    /// when no UI is up. See [`Renderer::render`].
    egui_renderer: egui_wgpu::Renderer,

    // ── Shop weapon preview: an offscreen turntable render of the selected gun,
    // sampled by egui as an image in the shop panel (see `render_weapon_preview`).
    /// Offscreen color target the gun renders into (sampled by egui).
    preview_color_view: wgpu::TextureView,
    /// Its depth buffer (own size, separate from the main frame's).
    preview_depth_view: wgpu::TextureView,
    /// The turntable MVP uniform + bind group (rewritten each preview frame).
    preview_clip: GpuClip,
    /// egui's handle to the preview color target, handed to the shop's `Image`.
    preview_tex_id: egui::TextureId,

    // ── Placeable props (the object palette). A generic textured-static-mesh
    // channel: distinct meshes keyed by catalog key, each drawn at N per-instance
    // transforms with a per-instance tint. Mirrors the enemy-weapon channel + adds
    // the tint uniform. See `upload_prop` / `set_prop_draws`.
    /// group(0) uniform layout for [`PropUniform`] (clip + tint), VERTEX+FRAGMENT.
    prop_uniform_layout: wgpu::BindGroupLayout,
    /// Depth-tested world pipeline for props (shader_prop.wgsl).
    prop_pipeline: wgpu::RenderPipeline,
    /// Uploaded prop meshes, keyed by catalog key.
    prop_meshes: HashMap<&'static str, GpuWeaponMesh>,
    /// Pooled per-draw uniforms, grown to the draw count each frame.
    prop_slots: Vec<GpuPropSlot>,
    /// This frame's prop draw list: `(slot index, mesh key)`.
    prop_draws: Vec<(usize, &'static str)>,
    /// The prop-preview turntable uniform (reuses the shared offscreen preview
    /// target + `preview_tex_id`; only one palette/shop preview renders per frame).
    prop_preview_slot: GpuPropSlot,

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
    surface_tint_pipeline: wgpu::RenderPipeline,
    highlight_mesh: Option<GpuMesh>,

    surface_tint_mesh: Option<GpuMesh>,
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
    /// REPEAT-wrapping sampler for placed props (many prop textures tile; the
    /// clamp `char_sampler` smears their edge texels). Nearest, like `char_sampler`,
    /// to keep the crisp N64 look.
    prop_sampler: wgpu::Sampler,
    /// One GPU mesh per character body id (uploaded once at startup, [`BODY_CATALOG`]
    /// order), and a reused pool of per-instance pose uniforms — `character_instance_
    /// count` of them are drawn this frame (one per hunter, or the single BUILD demo),
    /// each selecting its body via [`GpuCharacterInstance::body`].
    character_meshes: Vec<GpuCharacterMesh>,
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
    /// The fixed enemy spawn-point marker (a colored floor square). Reuses the
    /// depth-tested spark pipeline; drawn in BOTH modes so the builder can see where
    /// the wave comes in while authoring. Static — rebuilt each frame from the mark.
    marker_mesh: Option<GpuMesh>,

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

        // ── Shadow cube-array (distance maps): `MAX_SHADOW_LIGHTS` cubes packed as
        // one R32F 2D-array of 6 layers each. Rendered per (light, face); sampled in
        // the lit shaders as a cube-array. Created before the lighting bind group,
        // which binds it.
        let shadow_layers = (MAX_SHADOW_LIGHTS * 6) as u32;
        let shadow_cube = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow-cube-array"),
            size: wgpu::Extent3d {
                width: SHADOW_SIZE,
                height: SHADOW_SIZE,
                depth_or_array_layers: shadow_layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SHADOW_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        // One 2D render view per (light, face) — the colour target each face renders into.
        let shadow_face_views: Vec<wgpu::TextureView> = (0..shadow_layers)
            .map(|layer| {
                shadow_cube.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("shadow-face"),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: layer,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        // Cube-array view for sampling in the lit shaders.
        let shadow_sample_view = shadow_cube.create_view(&wgpu::TextureViewDescriptor {
            label: Some("shadow-cube-sample"),
            dimension: Some(wgpu::TextureViewDimension::CubeArray),
            ..Default::default()
        });
        // R32F isn't filterable, so a nearest (non-filtering) sampler; PCF is done by
        // multi-tapping in the shader.
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let shadow_depth_view = create_depth(&device, SHADOW_SIZE, SHADOW_SIZE);

        // Scene lighting uniform (point lights + ambient + flat/real flag), a single
        // FRAGMENT-visible uniform read by both region shaders, plus the shadow
        // cube-array + its sampler. Starts flat until the app pushes lights.
        let lighting_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lighting-uniform"),
            contents: bytemuck::cast_slice(&[LightingUniform::default()]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let lighting_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lighting-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::CubeArray,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
        let lighting_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lighting-bg"),
            layout: &lighting_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: lighting_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&shadow_sample_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&shadow_sampler),
                },
            ],
        });

        // A second, always-FLAT lighting bind group for the prop-preview turntable, so
        // a preview reads neutral (flat `shade`) regardless of the world's live
        // lighting — otherwise a preview would go dark when real lighting is on and the
        // model sits far from every light. Uses a private default (flat) uniform; the
        // shadow cube/sampler are bound only to satisfy the layout (never sampled, as
        // the flat branch returns before any light loop).
        let preview_lighting_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("preview-lighting-uniform"),
            contents: bytemuck::cast_slice(&[LightingUniform::default()]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let preview_lighting_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("preview-lighting-bg"),
            layout: &lighting_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: preview_lighting_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&shadow_sample_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&shadow_sampler),
                },
            ],
        });

        // Pipeline.
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("forward-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shader.wgsl").into()),
        });
        // Camera-only layout, shared by the overlay pipelines (highlight, stair ghost)
        // that don't consume lighting.
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("forward-layout"),
            bind_group_layouts: &[&camera_layout],
            push_constant_ranges: &[],
        });
        // The grid (checkerboard) pipeline is lit too, so it takes camera + lighting.
        let grid_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("forward-lit-layout"),
            bind_group_layouts: &[&camera_layout, &lighting_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("forward-pipeline"),
            layout: Some(&grid_layout),
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
            bind_group_layouts: &[&camera_layout, &material_layout, &lighting_layout],
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

        // ── Shadow pipeline: render region geometry from a light cube face, storing
        // linear light-distance into R32F with cutout discard. Reuses the material
        // group (group 1) for the per-zone alpha; its own face uniform is group 0.
        let face_uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow-face-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let shadow_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shader_shadow.wgsl").into()),
        });
        let shadow_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("shadow-layout"),
                bind_group_layouts: &[&face_uniform_layout, &material_layout],
                push_constant_ranges: &[],
            });
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow-pipeline"),
            layout: Some(&shadow_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shadow_shader,
                entry_point: Some("vs_main"),
                buffers: &[TexVertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shadow_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SHADOW_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::RED,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // Second-depth shadows: render only the BACK faces (those pointing away
                // from the light) so the stored occluder is each solid's FAR surface.
                // The receiver's own front face is then absent from the map, which
                // eliminates self-shadow acne (no big normal offset needed) and stops
                // light bleeding through wall seams. Costs mild peter-panning (shadows
                // start ~wall-thickness late) — the right trade for a hide-and-seek game.
                // Requires closed solids, which the CSG box geometry is. Box faces are
                // CCW-from-outside and rooms are carved by subtract, so the light-facing
                // interior surface is a front face → cull Front to keep the far side.
                cull_mode: Some(wgpu::Face::Front),
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
        // One pooled uniform + bind group per (light, face) so all faces can be written
        // up front and drawn in the same submit without aliasing a single buffer.
        let shadow_face_slots: Vec<(wgpu::Buffer, wgpu::BindGroup)> = (0..shadow_layers)
            .map(|_| {
                let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("shadow-face-uniform"),
                    contents: bytemuck::cast_slice(&[FaceUniform {
                        view_proj: Mat4::IDENTITY.to_cols_array_2d(),
                        light_pos: [0.0; 4],
                    }]),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                });
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("shadow-face-bg"),
                    layout: &face_uniform_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buf.as_entire_binding(),
                    }],
                });
                (buf, bind)
            })
            .collect();

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

        // ── Surface-tint pipeline: identical to the highlight above (same layout,
        // same vertex format, same alpha blending and depth behaviour) but with the
        // shader's cool low-alpha `fs_tint` colour instead of the warm yellow. Used to
        // wash the whole surface a tool is operating on — which for the freeform draw
        // tool is what disambiguates an edge or corner, where two or three faces meet
        // and the picked one would otherwise be invisible. Drawn *before* the highlight
        // so the outline reads on top of its own tint.
        let surface_tint_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("surface-tint-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &highlight_shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &highlight_shader,
                entry_point: Some("fs_tint"),
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
        // Prop sampler: REPEAT so tiling prop textures wrap instead of smearing their
        // edge texel (shelves/cabinets/tables), and LINEAR filtering for a smooth
        // look (props read better smoothed than the crisp-Nearest N64 weapons/chars).
        let prop_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("prop-sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let skinned_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("skinned-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shader_skinned.wgsl").into()),
        });
        let skinned_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("skinned-layout"),
            // group(3) = lighting so characters receive the level's light + shadows.
            bind_group_layouts: &[
                &camera_layout,
                &char_tex_layout,
                &char_uniform_layout,
                &lighting_layout,
            ],
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

        // ── Prop pipeline: placeable props (crate/barrel/furniture). Same textured
        // unlit look as the viewmodel, but group(0) is a PropUniform (clip + tint,
        // VERTEX+FRAGMENT visible) instead of the bare clip matrix, and it's drawn
        // in the depth-tested world pass (props are scene geometry, not an overlay).
        let prop_uniform_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("prop-uniform-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let prop_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("prop-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shader_prop.wgsl").into()),
        });
        let prop_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("prop-layout"),
                // group(2) = lighting so props receive the level's light + shadows.
                bind_group_layouts: &[
                    &prop_uniform_layout,
                    &viewmodel_tex_layout,
                    &lighting_layout,
                ],
                push_constant_ranges: &[],
            });
        let prop_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("prop-pipeline"),
            layout: Some(&prop_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &prop_shader,
                entry_point: Some("vs_main"),
                buffers: &[TexVertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &prop_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    // Alpha-blend so a tint.a < 1 can ghost a prop later; at tint.a
                    // == 1 (the normal case) this is an opaque write.
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None, // GLB winding varies; don't cull
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
                    // Alpha (over) blending, NOT additive: the fireball must OCCLUDE
                    // what's behind it (a dense GoldenEye explosion), not just add
                    // light — additive can never be opaque, so the wall always showed
                    // through. The shader boosts the alpha so the body reads solid
                    // while the cloud edges still feather out.
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

        // egui painter — targets the swapchain color format, no depth (menus draw
        // flat on top), single-sampled, no dithering. Textures/buffers are uploaded
        // per frame in `render` from the app's tessellated UI.
        let mut egui_renderer = egui_wgpu::Renderer::new(&device, config.format, None, 1, false);

        // Shop weapon-preview offscreen target: a square color texture (same format as
        // the swapchain so it reuses the viewmodel pipeline) + its own depth, plus a
        // turntable MVP uniform. Registered with egui so the shop can draw it as an
        // image. `make_clip` is a `&self` method, so build the clip inline here.
        let preview_color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("weapon-preview-color"),
            size: wgpu::Extent3d {
                width: PREVIEW_SIZE,
                height: PREVIEW_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let preview_color_view = preview_color.create_view(&wgpu::TextureViewDescriptor::default());
        let preview_depth_view = create_depth(&device, PREVIEW_SIZE, PREVIEW_SIZE);
        let preview_clip = {
            let clip_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("weapon-preview-clip"),
                contents: bytemuck::cast_slice(&[CameraUniform {
                    view_proj: Mat4::IDENTITY.to_cols_array_2d(),
                }]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            let clip_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("weapon-preview-clip"),
                layout: &camera_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: clip_buf.as_entire_binding(),
                }],
            });
            GpuClip { clip_buf, clip_bind }
        };
        // Prop-preview turntable uniform (clip + white tint), rewritten each preview
        // frame by `render_prop_preview`; renders into the same offscreen target.
        let prop_preview_slot = {
            let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("prop-preview-uniform"),
                contents: bytemuck::cast_slice(&[PropUniform {
                    view_proj: Mat4::IDENTITY.to_cols_array_2d(),
                    world: Mat4::IDENTITY.to_cols_array_2d(),
                    tint: [1.0, 1.0, 1.0, 1.0],
                }]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("prop-preview-uniform"),
                layout: &prop_uniform_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                }],
            });
            GpuPropSlot { buf, bind }
        };
        let preview_tex_id = egui_renderer.register_native_texture(
            &device,
            &preview_color_view,
            wgpu::FilterMode::Linear,
        );

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
            lighting_buf,
            lighting_bind_group,
            preview_lighting_bind_group,
            _shadow_cube: shadow_cube,
            shadow_face_views,
            shadow_depth_view,
            shadow_pipeline,
            shadow_face_slots,
            shadow_casters: Vec::new(),
            depth_view,
            egui_renderer,
            preview_color_view,
            preview_depth_view,
            preview_clip,
            preview_tex_id,
            prop_uniform_layout,
            prop_pipeline,
            prop_meshes: HashMap::new(),
            prop_slots: Vec::new(),
            prop_draws: Vec::new(),
            prop_preview_slot,
            regions: HashMap::new(),
            materials,
            _material_keepalive: material_keepalive,
            _material_buffers: material_buffers,
            grid_mode: false,
            highlight_pipeline,
            surface_tint_pipeline,
            highlight_mesh: None,
            surface_tint_mesh: None,
            stair_ghost_pipeline,
            stair_ghost_mesh: None,
            entity_pipeline,
            entity_mesh: None,
            skinned_pipeline,
            char_tex_layout,
            char_uniform_layout,
            char_sampler,
            prop_sampler,
            character_meshes: Vec::new(),
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
            marker_mesh: None,
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

    /// Set (or clear) the surface-tint mesh — a cool translucent wash marking which
    /// whole surface the active tool is operating on, drawn under the yellow highlight.
    pub fn set_surface_tint(&mut self, mesh: Option<&CpuMesh>) {
        self.surface_tint_mesh = match mesh {
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

    /// Upload one character body's geometry to the GPU at body id `index`: vertex/
    /// index buffers, one GPU texture per referenced image, and per-primitive texture
    /// bind groups. Call once per body at startup, in ascending `index` order; a
    /// hunter selects its body per frame via [`Renderer::set_character_instances`].
    pub fn upload_character(&mut self, index: usize, model: &SkinnedModel) {
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

        let mesh = GpuCharacterMesh {
            vertex_buf,
            index_buf,
            vertex_count: model.vertices.len() as u32,
            primitives,
            _textures: textures,
        };
        // Uploaded in ascending body-id order at startup, so `index` is either the
        // next slot (push) or an existing one being replaced (a reload).
        if index < self.character_meshes.len() {
            self.character_meshes[index] = mesh;
        } else {
            self.character_meshes.push(mesh);
        }
    }

    /// Build one pooled character-instance pose uniform + its bind group, plus a
    /// placeholder per-vertex blood-color buffer. `color_verts` starts at 0 so the
    /// first [`Renderer::set_character_instances`] write sizes the blood buffer to the
    /// body this slot actually draws (bodies differ in vertex count).
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
        // 1-vertex placeholder; resized to the real body on first use.
        let color_buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("char-blood"),
            contents: bytemuck::cast_slice(&[1.0f32; 3]),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });
        GpuCharacterInstance {
            body: 0,
            uniform_buf,
            uniform_bind,
            color_buf,
            color_verts: 1,
            color_clean: true,
        }
    }

    /// Set every character instance to draw this frame as `(body id, model, joint
    /// matrices, opacity, blood_colors)`. `blood_colors` is the flat per-vertex RGB
    /// (len = 3×that body's vertex_count) painted by shots — white where clean. Grows
    /// the reused instance pool to fit, re-sizes a slot's blood buffer when its body
    /// changes, writes each pose uniform + blood buffer, and records the count.
    /// `joints` is truncated/padded to `MAX_JOINTS`. No-op geometry-wise if no body
    /// mesh is uploaded.
    pub fn set_character_instances(&mut self, instances: &[(usize, Mat4, Vec<Mat4>, f32, &[f32])]) {
        self.character_instance_count = instances.len();
        while self.character_instances.len() < instances.len() {
            let inst = self.make_character_instance();
            self.character_instances.push(inst);
        }
        for (i, (body, model, joints, opacity, colors)) in instances.iter().enumerate() {
            // The blood buffer is a per-vertex VERTEX buffer, so it must cover the
            // body's whole vertex count or the shader reads past the end (an
            // indexed draw is not bounds-checked against it — the reads come back
            // as zeros, i.e. a BLACK character, with no validation error). A caller
            // that paints no blood passes an empty slice, so the size comes from
            // the body's mesh, not from the slice.
            let verts = if colors.is_empty() {
                self.character_meshes.get(*body).map_or(1, |m| m.vertex_count.max(1))
            } else {
                (colors.len() / 3) as u32
            };
            // Re-size this slot's blood buffer if it's now drawing a body with a
            // different vertex count (a pooled slot can switch bodies frame-to-frame).
            if self.character_instances[i].color_verts != verts {
                let white = vec![1.0f32; verts as usize * 3];
                let buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("char-blood"),
                    contents: bytemuck::cast_slice(&white),
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                });
                self.character_instances[i].color_buf = buf;
                self.character_instances[i].color_verts = verts;
                self.character_instances[i].color_clean = true;
            }
            self.character_instances[i].body = *body;
            let mut u = CharUniform {
                model: model.to_cols_array_2d(),
                opacity: [*opacity, 0.0, 0.0, 0.0],
                ..Default::default()
            };
            // A body with more joints than the uniform holds would be skinned by a
            // clamped index and draw as torn geometry, so say so rather than
            // truncate in silence (this is how the PD 30-joint rig hid behind a
            // 16-joint cap — see `MAX_JOINTS`).
            if joints.len() > MAX_JOINTS {
                log::warn!(
                    "character body {body} needs {} joints; the uniform holds {MAX_JOINTS} — it will draw torn",
                    joints.len()
                );
            }
            for (j, m) in joints.iter().take(MAX_JOINTS).enumerate() {
                u.joints[j] = m.to_cols_array_2d();
            }
            self.queue
                .write_buffer(&self.character_instances[i].uniform_buf, 0, bytemuck::cast_slice(&[u]));
            if !colors.is_empty() {
                self.queue.write_buffer(
                    &self.character_instances[i].color_buf,
                    0,
                    bytemuck::cast_slice(colors),
                );
                self.character_instances[i].color_clean = false;
            } else if !self.character_instances[i].color_clean {
                // This slot last drew a bloodied hunter and is now reused for an
                // unpainted body of the same size — scrub it back to white.
                let white = vec![1.0f32; verts as usize * 3];
                self.queue.write_buffer(
                    &self.character_instances[i].color_buf,
                    0,
                    bytemuck::cast_slice(&white),
                );
                self.character_instances[i].color_clean = true;
            }
        }
    }

    /// Remove all character geometry + instances (e.g. reload).
    pub fn clear_character(&mut self) {
        self.character_meshes.clear();
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
    fn build_weapon_mesh(
        &self,
        model: &TexturedModel,
        label: &str,
        sampler: &wgpu::Sampler,
    ) -> GpuWeaponMesh {
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
                            resource: wgpu::BindingResource::Sampler(sampler),
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

        // Bounding sphere (from the source vertices) so the preview can frame any
        // gun regardless of its raw scale.
        let (center, radius) = {
            let mut min = Vec3::splat(f32::INFINITY);
            let mut max = Vec3::splat(f32::NEG_INFINITY);
            for v in &model.vertices {
                let p = Vec3::from_array(v.pos);
                min = min.min(p);
                max = max.max(p);
            }
            if model.vertices.is_empty() {
                (Vec3::ZERO, 1.0)
            } else {
                let c = (min + max) * 0.5;
                ((c), (max - c).length().max(1e-3))
            }
        };

        GpuWeaponMesh {
            vertex_buf,
            index_buf,
            primitives,
            _textures: textures,
            center,
            radius,
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
        let mesh = self.build_weapon_mesh(model, label, &self.char_sampler);
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

    /// egui's texture handle for the weapon-preview image (handed to the shop's
    /// `Image` widget). Stable for the renderer's lifetime.
    pub fn weapon_preview_texture_id(&self) -> egui::TextureId {
        self.preview_tex_id
    }

    /// Render the weapon named `key` into the offscreen preview texture: framed by
    /// its bounding sphere and spun `angle` radians about the vertical axis (a
    /// turntable), lit by the same unlit/matcap viewmodel pipeline. Clears to a dark
    /// display background first, so an unknown weapon just shows an empty case. Call
    /// each frame the shop is open, **before** [`Self::render`] paints the egui pass
    /// that samples this texture.
    pub fn render_weapon_preview(&mut self, key: &str, angle: f32) {
        // Frame any gun regardless of its native GoldenEye-units scale: translate its
        // bounding-sphere centre to the origin, scale the sphere to unit radius, spin
        // about Y, and view from a slightly raised camera (the turntable tilt).
        let (center, radius) = self
            .enemy_weapon_meshes
            .get(key)
            .map(|m| (m.center, m.radius))
            .unwrap_or((Vec3::ZERO, 1.0));
        let model = Mat4::from_rotation_y(angle)
            * Mat4::from_scale(Vec3::splat(1.0 / radius))
            * Mat4::from_translation(-center);
        let view = Mat4::look_at_rh(Vec3::new(0.0, 1.05, 3.0), Vec3::ZERO, Vec3::Y);
        let proj = Mat4::perspective_rh(38f32.to_radians(), 1.0, 0.05, 100.0);
        let clip = proj * view * model;
        self.queue.write_buffer(
            &self.preview_clip.clip_buf,
            0,
            bytemuck::cast_slice(&[CameraUniform {
                view_proj: clip.to_cols_array_2d(),
            }]),
        );

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("weapon-preview-encoder"),
            });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("weapon-preview-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.preview_color_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Opaque dark "display case" background (avoids egui alpha
                        // compositing fringes and reads as a product display).
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.055,
                            b: 0.065,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.preview_depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if let Some(mesh) = self.enemy_weapon_meshes.get(key) {
                rp.set_pipeline(&self.viewmodel_pipeline);
                rp.set_bind_group(0, &self.preview_clip.clip_bind, &[]);
                rp.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
                rp.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                for p in &mesh.primitives {
                    rp.set_bind_group(1, &p.tex_bind, &[]);
                    rp.draw_indexed(p.index_start..(p.index_start + p.index_count), 0, 0..1);
                }
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
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

    // ── Placeable props (the object palette) ────────────────────────────────

    /// Add one prop mesh to the render library, keyed by its catalog key. Call once
    /// per catalog entry at startup; draw any number of instances per frame via
    /// [`Renderer::set_prop_draws`].
    pub fn upload_prop(&mut self, key: &'static str, model: &TexturedModel) {
        // Props use the REPEAT sampler (many prop textures tile — a clamp sampler
        // smears the edge texel into black bars on shelves/cabinets/tables).
        let mesh = self.build_weapon_mesh(model, "prop", &self.prop_sampler);
        self.prop_meshes.insert(key, mesh);
    }

    /// Set this frame's prop draw list: `view_proj` (the shared camera clip matrix)
    /// plus `(key, model→world, tint rgba)` per placed prop. Grows the pooled uniform
    /// slots, writes each (clip = `view_proj · world`, plus the world matrix for
    /// lighting), and records the draw list; a draw for an unknown key is skipped in
    /// the pass.
    pub fn set_prop_draws(&mut self, view_proj: Mat4, draws: &[(&'static str, Mat4, [f32; 4])]) {
        while self.prop_slots.len() < draws.len() {
            let buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("prop-uniform"),
                contents: bytemuck::cast_slice(&[PropUniform {
                    view_proj: Mat4::IDENTITY.to_cols_array_2d(),
                    world: Mat4::IDENTITY.to_cols_array_2d(),
                    tint: [1.0, 1.0, 1.0, 1.0],
                }]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("prop-uniform"),
                layout: &self.prop_uniform_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                }],
            });
            self.prop_slots.push(GpuPropSlot { buf, bind });
        }
        self.prop_draws.clear();
        for (i, (key, world, tint)) in draws.iter().enumerate() {
            self.queue.write_buffer(
                &self.prop_slots[i].buf,
                0,
                bytemuck::cast_slice(&[PropUniform {
                    view_proj: (view_proj * *world).to_cols_array_2d(),
                    world: world.to_cols_array_2d(),
                    tint: *tint,
                }]),
            );
            self.prop_draws.push((i, key));
        }
    }

    /// egui's texture handle for the prop-preview image. Shares the offscreen target
    /// with the weapon preview (only one panel previews per frame).
    pub fn prop_preview_texture_id(&self) -> egui::TextureId {
        self.preview_tex_id
    }

    /// Render prop `key` into the offscreen preview texture as a turntable spun
    /// `angle` radians, framed by its bounding sphere. Mirrors
    /// [`Self::render_weapon_preview`] but uses the prop pipeline (white tint). Call
    /// each frame the palette is open, before [`Self::render`].
    pub fn render_prop_preview(&mut self, key: &str, angle: f32) {
        let (center, radius) = self
            .prop_meshes
            .get(key)
            .map(|m| (m.center, m.radius))
            .unwrap_or((Vec3::ZERO, 1.0));
        let model = Mat4::from_rotation_y(angle)
            * Mat4::from_scale(Vec3::splat(1.0 / radius))
            * Mat4::from_translation(-center);
        let view = Mat4::look_at_rh(Vec3::new(0.0, 1.05, 3.0), Vec3::ZERO, Vec3::Y);
        let proj = Mat4::perspective_rh(38f32.to_radians(), 1.0, 0.05, 100.0);
        let clip = proj * view * model;
        self.queue.write_buffer(
            &self.prop_preview_slot.buf,
            0,
            bytemuck::cast_slice(&[PropUniform {
                view_proj: clip.to_cols_array_2d(),
                world: model.to_cols_array_2d(),
                tint: [1.0, 1.0, 1.0, 1.0],
            }]),
        );
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("prop-preview-encoder"),
            });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("prop-preview-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.preview_color_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.055,
                            b: 0.065,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.preview_depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if let Some(mesh) = self.prop_meshes.get(key) {
                rp.set_pipeline(&self.prop_pipeline);
                rp.set_bind_group(0, &self.prop_preview_slot.bind, &[]);
                rp.set_bind_group(2, &self.preview_lighting_bind_group, &[]);
                rp.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
                rp.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                for p in &mesh.primitives {
                    rp.set_bind_group(1, &p.tex_bind, &[]);
                    rp.draw_indexed(p.index_start..(p.index_start + p.index_count), 0, 0..1);
                }
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Add one enemy weapon's gun mesh to the render library, keyed by weapon name
    /// (A3, arsenal). Call once per arsenal weapon at startup; draw any number of
    /// them per frame via [`Renderer::set_enemy_weapon_draws`].
    pub fn upload_enemy_weapon(&mut self, key: &'static str, model: &TexturedModel) {
        let mesh = self.build_weapon_mesh(model, "enemy-gun", &self.char_sampler);
        self.enemy_weapon_meshes.insert(key, mesh);
    }

    /// Add one enemy weapon's muzzle-flash mesh to the render library, keyed by
    /// weapon name. Drawn per frame via [`Renderer::set_enemy_muzzle_draws`].
    pub fn upload_enemy_muzzle(&mut self, key: &'static str, model: &TexturedModel) {
        let mesh = self.build_weapon_mesh(model, "enemy-muzzle", &self.char_sampler);
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
    pub fn set_marker_mesh(&mut self, mesh: Option<&ColoredMesh>) {
        self.marker_mesh = match mesh {
            Some(m) if !m.indices.is_empty() => Some(GpuMesh::upload_colored(&self.device, m)),
            _ => None,
        };
    }

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
        let vbytes: &[u8] = bytemuck::cast_slice(&mesh.vertices);
        let ibytes: &[u8] = bytemuck::cast_slice(&mesh.indices);

        // Fast path: the region already has buffers big enough — overwrite in
        // place with `write_buffer` (no allocation) instead of reallocating a
        // fresh VERTEX/INDEX buffer on every BUILD edit.
        if let Some(existing) = self.regions.get_mut(&region_id) {
            if (vbytes.len() as u64) <= existing.vertex_cap
                && (ibytes.len() as u64) <= existing.index_cap
            {
                self.queue.write_buffer(&existing.vertex_buf, 0, vbytes);
                self.queue.write_buffer(&existing.index_buf, 0, ibytes);
                existing.index_count = mesh.indices.len() as u32;
                existing.groups = mesh.groups.clone();
                return;
            }
        }

        // Slow path: (re)allocate with headroom so subsequent edits that grow the
        // mesh a little still hit the fast path. COPY_DST makes them writable.
        let vertex_cap = (vbytes.len() as u64).next_power_of_two().max(4096);
        let index_cap = (ibytes.len() as u64).next_power_of_two().max(4096);
        let vertex_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("region-tex-vertices"),
            size: vertex_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let index_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("region-tex-indices"),
            size: index_cap,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&vertex_buf, 0, vbytes);
        self.queue.write_buffer(&index_buf, 0, ibytes);
        self.regions.insert(
            region_id,
            TexturedRegion {
                vertex_buf,
                index_buf,
                vertex_cap,
                index_cap,
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

    /// Upload this frame's scene lighting into the shared lighting uniform read by
    /// the region shaders. `lights` = `(world_pos_metres, colour_rgb, intensity,
    /// range_metres, shadow_index)` per active point light (capped at [`MAX_LIGHTS`]);
    /// `shadow_index` is the light's shadow-cube slot (0..`MAX_SHADOW_LIGHTS`) or a
    /// negative value for a non-caster. `ambient` = `(colour_rgb, level)`; `real`
    /// selects point lighting vs the legacy flat look. Call each frame before render.
    pub fn set_lighting(
        &mut self,
        lights: &[(Vec3, [f32; 3], f32, f32, i32)],
        ambient: ([f32; 3], f32),
        real: bool,
    ) {
        let mut u = LightingUniform::default();
        let ([ar, ag, ab], level) = ambient;
        // Premultiply ambient colour by its level; w carries the flat flag (1 = flat).
        u.ambient = [ar * level, ag * level, ab * level, if real { 0.0 } else { 1.0 }];
        let n = lights.len().min(MAX_LIGHTS);
        u.count[0] = n as u32;
        for (i, (pos, col, intensity, range, shadow)) in lights.iter().take(n).enumerate() {
            u.lights[i] = GpuLight {
                pos_range: [pos.x, pos.y, pos.z, *range],
                color_intensity: [col[0], col[1], col[2], *intensity],
                params: [*shadow as f32, 0.0, 0.0, 0.0],
            };
        }
        self.queue
            .write_buffer(&self.lighting_buf, 0, bytemuck::cast_slice(&[u]));
    }

    /// Set this frame's shadow casters `(world_pos_metres, range_metres)`, in
    /// shadow-cube-index order (index 0 = first entry). Capped at
    /// [`MAX_SHADOW_LIGHTS`]; extra casters are dropped. Their per-light
    /// `shadow_index` in [`Self::set_lighting`] must match this ordering. The shadow
    /// cubes are rendered from these in [`Renderer::render`].
    pub fn set_shadow_casters(&mut self, casters: &[(Vec3, f32)]) {
        self.shadow_casters.clear();
        self.shadow_casters
            .extend(casters.iter().take(MAX_SHADOW_LIGHTS).copied());
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

    pub fn render(&mut self, view_proj: Mat4, egui: Option<EguiFrame>) {
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

        // ── Shadow pass: fill each caster's distance cube (6 faces) from the region
        // geometry, before the forward pass that samples them. Skipped when nothing
        // casts shadows. Face uniforms are all written first (distinct buffers) so the
        // in-encoder passes don't alias one buffer.
        if !self.shadow_casters.is_empty() {
            for (li, &(pos, range)) in self.shadow_casters.iter().enumerate() {
                for face in 0..6usize {
                    let vp = cube_face_view_proj(pos, range, face);
                    self.queue.write_buffer(
                        &self.shadow_face_slots[li * 6 + face].0,
                        0,
                        bytemuck::cast_slice(&[FaceUniform {
                            view_proj: vp.to_cols_array_2d(),
                            light_pos: [pos.x, pos.y, pos.z, range],
                        }]),
                    );
                }
            }
            for li in 0..self.shadow_casters.len() {
                for face in 0..6usize {
                    let slot = li * 6 + face;
                    let mut sp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("shadow-pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &self.shadow_face_views[slot],
                            resolve_target: None,
                            ops: wgpu::Operations {
                                // Clear to max distance (≥ range → nothing occludes).
                                load: wgpu::LoadOp::Clear(wgpu::Color {
                                    r: 1.0,
                                    g: 0.0,
                                    b: 0.0,
                                    a: 0.0,
                                }),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: &self.shadow_depth_view,
                            depth_ops: Some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(1.0),
                                store: wgpu::StoreOp::Store,
                            }),
                            stencil_ops: None,
                        }),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    sp.set_pipeline(&self.shadow_pipeline);
                    sp.set_bind_group(0, &self.shadow_face_slots[slot].1, &[]);
                    for m in self.regions.values() {
                        sp.set_vertex_buffer(0, m.vertex_buf.slice(..));
                        sp.set_index_buffer(m.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                        for g in &m.groups {
                            if let Some(bg) = self
                                .materials
                                .get(g.scheme as usize)
                                .and_then(|z| z[g.zone as usize].as_ref())
                            {
                                sp.set_bind_group(1, bg, &[]);
                                sp.draw_indexed(g.start..(g.start + g.count), 0, 0..1);
                            }
                        }
                    }
                }
            }
        }
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
            // 1) Opaque region meshes — grid (checkerboard) or textured view. Both
            // are lit: the shared lighting uniform sits at group(1) for the grid
            // pipeline and group(2) for the textured pipeline.
            rp.set_bind_group(0, &self.camera_bind_group, &[]);
            if self.grid_mode {
                rp.set_pipeline(&self.pipeline);
                rp.set_bind_group(1, &self.lighting_bind_group, &[]);
                for m in self.regions.values() {
                    rp.set_vertex_buffer(0, m.vertex_buf.slice(..));
                    rp.set_index_buffer(m.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    rp.draw_indexed(0..m.index_count, 0, 0..1);
                }
            } else {
                rp.set_pipeline(&self.textured_pipeline);
                rp.set_bind_group(2, &self.lighting_bind_group, &[]);
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

            // 2.05) The enemy spawn-point marker (colored floor square), same
            // depth-tested unlit pipeline as sparks; drawn in both BUILD and HUNT.
            if let Some(mk) = &self.marker_mesh {
                rp.set_pipeline(&self.spark_pipeline);
                rp.set_bind_group(0, &self.camera_bind_group, &[]);
                rp.set_vertex_buffer(0, mk.vertex_buf.slice(..));
                rp.set_index_buffer(mk.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                rp.draw_indexed(0..mk.index_count, 0, 0..1);
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
            // joints/model; group(1)=texture per primitive. Each instance selects its
            // body's mesh (bodies vary), so vertex/index buffers are re-bound per
            // instance — cheap at hunter counts.
            if !self.character_meshes.is_empty() {
                rp.set_pipeline(&self.skinned_pipeline);
                rp.set_bind_group(0, &self.camera_bind_group, &[]);
                for inst in self.character_instances.iter().take(self.character_instance_count) {
                    let Some(ch) = self.character_meshes.get(inst.body) else {
                        continue; // unknown body id → skip (shouldn't happen)
                    };
                    rp.set_vertex_buffer(0, ch.vertex_buf.slice(..));
                    rp.set_index_buffer(ch.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    rp.set_bind_group(2, &inst.uniform_bind, &[]);
                    rp.set_bind_group(3, &self.lighting_bind_group, &[]);
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
            // 2.35) Placed props (crate/barrel/furniture) — world-space, depth-tested
            // vs the scene. One draw per placed instance, mesh looked up by key, with
            // a per-instance tint (white here; the darken-on-hit uses it in M3).
            for (slot_idx, key) in &self.prop_draws {
                let (Some(w), Some(slot)) =
                    (self.prop_meshes.get(key), self.prop_slots.get(*slot_idx))
                else {
                    continue;
                };
                rp.set_pipeline(&self.prop_pipeline);
                rp.set_bind_group(0, &slot.bind, &[]);
                rp.set_bind_group(2, &self.lighting_bind_group, &[]);
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

            // 2.9) Surface tint (translucent wash over the whole surface a tool acts
            // on). Before the highlight so the outline reads on top of its own tint.
            if let Some(t) = &self.surface_tint_mesh {
                rp.set_pipeline(&self.surface_tint_pipeline);
                rp.set_bind_group(0, &self.camera_bind_group, &[]);
                rp.set_vertex_buffer(0, t.vertex_buf.slice(..));
                rp.set_index_buffer(t.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                rp.draw_indexed(0..t.index_count, 0, 0..1);
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

        // ── egui pass: menus (shop / inventory) on top of everything, when the app
        // handed us a UI frame this frame. Upload the frame's texture deltas + vertex/
        // index buffers, then paint into the swapchain view (color loaded, no depth).
        if let Some(egui) = egui {
            let screen = egui_wgpu::ScreenDescriptor {
                size_in_pixels: [self.config.width, self.config.height],
                pixels_per_point: egui.pixels_per_point,
            };
            for (id, delta) in &egui.textures_delta.set {
                self.egui_renderer
                    .update_texture(&self.device, &self.queue, *id, delta);
            }
            self.egui_renderer.update_buffers(
                &self.device,
                &self.queue,
                &mut encoder,
                &egui.paint_jobs,
                &screen,
            );
            {
                let mut rp = encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("egui-pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view_tex,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    })
                    // egui-wgpu's `render` wants a `RenderPass<'static>`; drop the
                    // encoder-borrow lifetime (the pass still ends at scope exit).
                    .forget_lifetime();
                self.egui_renderer.render(&mut rp, &egui.paint_jobs, &screen);
            }
            // Free textures egui dropped this frame (after the pass that used them).
            for id in &egui.textures_delta.free {
                self.egui_renderer.free_texture(id);
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

/// View-projection for one cube face of a point-light shadow, looking from `pos`
/// along the face's major axis with a 90° frustum out to `far` (the light range).
/// Face order matches the cube-array layer order (`+X, -X, +Y, -Y, +Z, -Z`) and the
/// sampler's cube convention. If shadows appear mirrored/on the wrong face, this
/// up-vector / handedness set is the first thing to adjust.
fn cube_face_view_proj(pos: Vec3, far: f32, face: usize) -> Mat4 {
    let (dir, up) = match face {
        0 => (Vec3::X, Vec3::NEG_Y),
        1 => (Vec3::NEG_X, Vec3::NEG_Y),
        2 => (Vec3::Y, Vec3::Z),
        3 => (Vec3::NEG_Y, Vec3::NEG_Z),
        4 => (Vec3::Z, Vec3::NEG_Y),
        _ => (Vec3::NEG_Z, Vec3::NEG_Y),
    };
    let view = Mat4::look_at_rh(pos, pos + dir, up);
    let proj = Mat4::perspective_rh(std::f32::consts::FRAC_PI_2, 1.0, 0.05, far.max(0.2));
    // The (dir, up) basis above is the canonical OpenGL cube-shadow set, which assumes
    // a bottom-left render-target origin. wgpu (like Vulkan/D3D) renders with a top-left
    // origin, so each face lands vertically flipped vs the cube-sampling convention the
    // receive side uses for its `dir` lookup — mirroring shadows on the vertical axis.
    // Flip clip-space Y to compensate. Safe with no winding fix because the shadow
    // pipeline uses `cull_mode: None`.
    let y_flip = Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0));
    y_flip * proj * view
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
