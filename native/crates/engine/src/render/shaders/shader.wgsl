// Phase 0 forward shader: transform by camera view-proj, shade with a single
// fixed directional light (Lambert + ambient). Deliberately minimal — the
// lighting/shadow-bake subsystem (deferred) replaces this later.

struct Camera {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    // UV is present in the shared TexVertex layout but unused in grid view.
    @location(2) uv: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    // World-space position (the mesh is already in world/meter space — no model
    // matrix — so the vertex position doubles as world position). Used for the
    // checkerboard, which needs stable world coords, not screen coords.
    @location(1) world_pos: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(in.pos, 1.0);
    out.normal = in.normal;
    out.world_pos = in.pos;
    return out;
}

// One world-tile (WT) in meters — the authoring grid unit (WORLD_SCALE). A
// checker cell = 1 WT, so each stair step / brush step reads as one square.
const CHECK_CELL: f32 = 0.25;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let l = normalize(vec3<f32>(0.4, 1.0, 0.6));
    // Two-sided: the region mesh is now drawn with culling off and some geometry
    // (stairs, structures) is single-winding — abs keeps both faces lit.
    let ndl = abs(dot(normalize(in.normal), l));

    // 3D checkerboard in world space: on any axis-aligned face the constant axis
    // drops out, leaving a 2D checker. A tiny bias keeps faces sitting exactly on
    // a cell boundary (e.g. the y=0 floor) from parity-flickering.
    let cell = floor((in.world_pos + vec3<f32>(0.0005)) / CHECK_CELL);
    let parity = (i32(cell.x) + i32(cell.y) + i32(cell.z)) & 1;
    let base = select(vec3<f32>(0.72, 0.74, 0.82), vec3<f32>(0.44, 0.46, 0.54), parity == 1);

    let lit = base * (0.25 + 0.75 * ndl);
    return vec4<f32>(lit, 1.0);
}
