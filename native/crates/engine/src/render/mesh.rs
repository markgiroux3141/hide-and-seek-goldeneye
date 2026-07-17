//! CPU-side mesh data and its GPU upload. The CSG core and the glTF loader both
//! produce [`CpuMesh`]; the renderer turns it into a [`GpuMesh`] to draw.

use wgpu::util::DeviceExt;

/// One interleaved vertex: position + normal. Matches the WGSL `VertexInput`.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
}

impl Vertex {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
    };
}

/// Renderer-agnostic mesh: what the CSG core and glTF loader emit.
#[derive(Clone, Default)]
pub struct CpuMesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl CpuMesh {
    /// Build from the CSG core's `(positions, normals, indices)` triple.
    pub fn from_csg(positions: &[f32], normals: &[f32], indices: &[u32]) -> CpuMesh {
        let vertices = positions
            .chunks_exact(3)
            .zip(normals.chunks_exact(3))
            .map(|(p, n)| Vertex {
                pos: [p[0], p[1], p[2]],
                normal: [n[0], n[1], n[2]],
            })
            .collect();
        CpuMesh {
            vertices,
            indices: indices.to_vec(),
        }
    }
}

/// One interleaved vertex for the textured region/structure pipeline:
/// position + normal + tile-unit UV. Matches `shader_textured.wgsl` and the
/// checkerboard `shader.wgsl` (which ignores the UV).
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TexVertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    /// Per-vertex color (glTF `COLOR_0`), multiplied onto the sampled texel. The
    /// GoldenEye weapon GLBs use a **palette texture × vertex color** scheme (a
    /// 1-row palette strip picked per-vertex, tinted by this), so dropping it made
    /// palette-white primitives render solid white. Region meshes have no vertex
    /// colors → they use white (`1,1,1,1`), a no-op multiply.
    pub color: [f32; 4],
}

impl TexVertex {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<TexVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32x4],
    };

    /// A vertex with no tint (white `COLOR_0`) — for meshes without vertex colors
    /// (regions/structures), where the color multiply is a no-op.
    pub fn new(pos: [f32; 3], normal: [f32; 3], uv: [f32; 2]) -> Self {
        TexVertex { pos, normal, uv, color: [1.0, 1.0, 1.0, 1.0] }
    }
}

/// A contiguous run of indices sharing one (scheme, zone) material. Drawn as one
/// indexed range binding `materials[scheme][zone]`. Scheme is per-triangle (via
/// the owning brush) so one region can mix schemes — e.g. a room and the room
/// beyond its door.
#[derive(Clone, Copy, Debug)]
pub struct ZoneGroup {
    pub scheme: u16,
    pub zone: u8,
    /// Offset into the index buffer (in indices, not triangles).
    pub start: u32,
    pub count: u32,
}

/// A classified, UV'd region/structure mesh: un-indexed vertices (3 per triangle)
/// plus an index buffer sorted so each (scheme, zone) forms one contiguous
/// [`ZoneGroup`]. The output of [`crate::render::uv_zones`].
#[derive(Clone, Default)]
pub struct TexturedMesh {
    pub vertices: Vec<TexVertex>,
    pub indices: Vec<u32>,
    pub groups: Vec<ZoneGroup>,
}

/// One interleaved vertex for the **skinned character** pipeline: position +
/// tile-unit UV + 4 joint indices + 4 skin weights. Deliberately has **no
/// normal** — the GoldenEye character GLBs carry no `NORMAL` attribute and render
/// unlit (N64 look), so lighting is impossible and unwanted. Matches
/// `shader_skinned.wgsl`.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SkinVertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub joints: [u32; 4],
    pub weights: [f32; 4],
}

impl SkinVertex {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<SkinVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![
            0 => Float32x3, 1 => Float32x2, 2 => Uint32x4, 3 => Float32x4
        ],
    };
}

/// One interleaved position + RGB color vertex — for the unlit gizmo overlay
/// (each handle a different color in one mesh). Matches `gizmo.wgsl`.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ColorVertex {
    pub pos: [f32; 3],
    pub color: [f32; 3],
}

impl ColorVertex {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<ColorVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
    };
}

/// A colored mesh (position + per-vertex color), for the gizmo overlay.
#[derive(Clone, Default)]
pub struct ColoredMesh {
    pub vertices: Vec<ColorVertex>,
    pub indices: Vec<u32>,
}

/// One screen-space HUD vertex: NDC position + atlas UV. Drawn un-indexed (6 verts
/// per quad) by the HUD pipeline, sampling the glyph atlas. No depth, alpha-keyed.
/// Matches `shader_hud.wgsl`. Positions are already in clip space (−1..1), so the
/// HUD needs no camera or projection.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct HudVertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
}

impl HudVertex {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<HudVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
    };
}

/// GPU-resident mesh: vertex + index buffers ready to draw.
pub struct GpuMesh {
    pub vertex_buf: wgpu::Buffer,
    pub index_buf: wgpu::Buffer,
    pub index_count: u32,
}

impl GpuMesh {
    pub fn upload(device: &wgpu::Device, mesh: &CpuMesh) -> GpuMesh {
        let vertex_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mesh-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mesh-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        GpuMesh {
            vertex_buf,
            index_buf,
            index_count: mesh.indices.len() as u32,
        }
    }

    /// Upload a colored mesh (position + per-vertex color) — for the gizmo.
    pub fn upload_colored(device: &wgpu::Device, mesh: &ColoredMesh) -> GpuMesh {
        let vertex_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("gizmo-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("gizmo-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        GpuMesh {
            vertex_buf,
            index_buf,
            index_count: mesh.indices.len() as u32,
        }
    }
}
