// Point-light shadow pass. Renders region/structure geometry from one cube face of
// one shadow-casting light, storing the fragment's LINEAR distance to the light
// (normalised by the light's range) into an R32F cube-array face. The lit shaders
// later sample this distance cube by direction and compare it against a fragment's
// own light-distance to decide occlusion.
//
// Storing world-space distance (not per-face NDC depth) means the stored value is
// orientation-independent — only the cube-face *mapping* has to match the sampler's
// convention, which keeps the receive side simple. Cutout zones (railings) sample
// their albedo alpha and `discard`, so their shadows are gappy just like the lit pass.

struct Face {
    view_proj: mat4x4<f32>,
    // xyz = light world position (m); w = range (the shadow far plane).
    light_pos: vec4<f32>,
};
@group(0) @binding(0) var<uniform> face: Face;

// Same material group layout as the textured pass (texture + sampler + repeat), so
// the per-zone material bind groups can be reused unchanged for the cutout test.
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;
struct Material { params: vec4<f32> };
@group(1) @binding(2) var<uniform> material: Material;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
};

@vertex
fn vs_main(
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
) -> VsOut {
    var out: VsOut;
    // Region/structure meshes are already in world/metre space (no model matrix).
    out.clip = face.view_proj * vec4<f32>(pos, 1.0);
    out.world_pos = pos;
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Cutout: railings (and any alpha-keyed zone) drop their transparent texels so
    // the occluder — and thus its shadow — has the same holes as the visible mesh.
    let c = textureSample(tex, samp, in.uv * material.params.x);
    if (c.a < 0.5) {
        discard;
    }
    let dist = length(in.world_pos - face.light_pos.xyz) / max(face.light_pos.w, 0.0001);
    return vec4<f32>(dist, 0.0, 0.0, 1.0);
}
