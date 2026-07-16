// Textured region/structure shader: sample a per-zone BMP × a single fixed
// directional light. Two-sided lighting (abs of N·L) because the mesh is drawn
// with culling off and some hand-emitted geometry (stairs, structures) is
// single-winding — this keeps both faces lit rather than one going black.

struct Camera {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct Material {
    // .x = tile-unit → texture-space scale (JS `texture.repeat`); UVs arrive in
    // WT. Packed as a vec4 so the Rust-side uniform (16 bytes) matches the WGSL
    // std140 layout exactly (a bare `f32 + vec3` pad would round up to 32).
    params: vec4<f32>,
};
@group(1) @binding(2) var<uniform> material: Material;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(in.pos, 1.0);
    out.normal = in.normal;
    out.uv = in.uv;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let l = normalize(vec3<f32>(0.4, 1.0, 0.6));
    let ndl = abs(dot(normalize(in.normal), l));
    let c = textureSample(tex, samp, in.uv * material.params.x);
    // Alpha-test (JS `alphaTest: 0.5`): cut out the transparent texels of the
    // railing texture. Opaque zone textures decode to alpha 1, so they're
    // unaffected — and discard is order-independent, needing no blend/sort.
    if (c.a < 0.5) {
        discard;
    }
    let lit = c.rgb * (0.25 + 0.75 * ndl);
    return vec4<f32>(lit, 1.0);
}
