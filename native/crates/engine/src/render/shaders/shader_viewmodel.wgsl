// First-person weapon viewmodel: a static gun mesh placed in view space and
// projected by a single clip matrix (projection · viewmodel-transform), drawn in
// a depth-cleared overlay pass so it's always on top and never clips walls.
// Unlit textured — the GoldenEye weapon skins are N64-style, no lighting (matches
// the skinned-character shader).

struct Clip {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> clip: Clip;

@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    out.pos = clip.view_proj * vec4<f32>(pos, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Palette texel × per-vertex color (the GoldenEye N64 shading): a white
    // palette entry tinted by the vertex color gives the real (e.g. dark metal)
    // surface color. Region meshes carry white vertex colors → no-op.
    return textureSample(tex, samp, in.uv) * in.color;
}
