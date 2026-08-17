// Skinned-character shader: linear-blend skinning (LBS) in the vertex stage,
// **unlit** textured output in the fragment stage. The GoldenEye character GLBs
// carry no NORMAL attribute (N64 look), so there is deliberately no lighting —
// the base-color texture is emitted as-is, matching the JS reference intent.

struct Camera {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

// 32, not 16: the GoldenEye rig is 15 bones, but a Perfect Dark body declares 30
// joints — `Bone_1..15` plus `Blend_1..15`, PD's midpoint frames, which are real
// joints on the exported rig (see `HANDOFF_PD_ASSETS.md`, conversion decision 3).
// At 16 every `Blend_*` index was out of range; WGSL clamps an out-of-bounds
// uniform-array index to the last element, so every blend-weighted vertex was
// skinned by `Bone_16`'s matrix and the body tore into a fan of stretched
// triangles. Must stay in step with `renderer::MAX_JOINTS`.
const MAX_JOINTS: u32 = 32u;
struct Char {
    // World placement of the whole character (GE-scale → metres + position).
    model: mat4x4<f32>,
    // Skinning matrices: global(joint) · inverseBind(joint). Bind pose = identity.
    joints: array<mat4x4<f32>, MAX_JOINTS>,
    // .x = whole-character opacity (Track A death fade), 1 = opaque.
    // vec4 to keep the 16-byte std140 alignment after the joint array.
    opacity: vec4<f32>,
};
@group(2) @binding(0) var<uniform> ch: Char;

// ── Lighting (group 3): the same uniform + shadow cubes the walls/props use. The
// skinned vertices carry NO normal (N64 rip), so characters can't do directional
// diffuse — instead they take the local light LEVEL (ambient + distance falloff)
// and RECEIVE point-light shadows, so a hunter darkens in shadow and brightens near
// a lamp. They don't cast (not rendered into the shadow cubes).
struct Light {
    pos_range: vec4<f32>,
    color_intensity: vec4<f32>,
    params: vec4<f32>,           // x = shadow cube index (< 0 = no shadow)
};
struct Lighting {
    ambient: vec4<f32>,          // rgb = ambient × level, w = flat flag (1 = flat)
    count: vec4<u32>,
    lights: array<Light, 32>,
};
@group(3) @binding(0) var<uniform> lighting: Lighting;
@group(3) @binding(1) var shadow_maps: texture_cube_array<f32>;
@group(3) @binding(2) var shadow_samp: sampler;

// Normal-free shadow test. Characters aren't in the shadow cubes (back-face pass),
// so there's no self-shadow acne to fight — a small constant bias + a 5-tap PCF is
// all that's needed. No normal offset (there's no normal).
fn shadow_factor_pos(idx: i32, frag: vec3<f32>, lp: vec3<f32>, range: f32) -> f32 {
    let to_frag = frag - lp;
    let dist = length(to_frag) / max(range, 0.0001);
    let dir = normalize(to_frag);
    let bias = 0.01;
    var t = cross(dir, vec3<f32>(0.0, 1.0, 0.0));
    if (dot(t, t) < 1e-4) { t = vec3<f32>(1.0, 0.0, 0.0); }
    t = normalize(t);
    let b = cross(dir, t);
    let e = 0.02;
    var lit = step(dist - bias, textureSampleLevel(shadow_maps, shadow_samp, dir, idx, 0.0).r);
    lit = lit + step(dist - bias, textureSampleLevel(shadow_maps, shadow_samp, dir + t * e, idx, 0.0).r);
    lit = lit + step(dist - bias, textureSampleLevel(shadow_maps, shadow_samp, dir - t * e, idx, 0.0).r);
    lit = lit + step(dist - bias, textureSampleLevel(shadow_maps, shadow_samp, dir + b * e, idx, 0.0).r);
    lit = lit + step(dist - bias, textureSampleLevel(shadow_maps, shadow_samp, dir - b * e, idx, 0.0).r);
    return lit / 5.0;
}

// Local light level at a world position, with no directional term (ndl = 1). Flat
// mode (ambient.w == 1) returns full-bright so the character looks exactly as it did
// before lighting existed.
fn shade_pos(world_pos: vec3<f32>) -> vec3<f32> {
    if (lighting.ambient.w > 0.5) {
        return vec3<f32>(1.0);
    }
    var lit = lighting.ambient.rgb;
    let count = lighting.count.x;
    for (var i = 0u; i < count; i = i + 1u) {
        let lp = lighting.lights[i].pos_range.xyz;
        let range = max(lighting.lights[i].pos_range.w, 0.0001);
        let dist = length(lp - world_pos);
        let a = clamp(1.0 - dist / range, 0.0, 1.0);
        let falloff = a * a;
        var shadow = 1.0;
        let sidx = i32(lighting.lights[i].params.x);
        if (sidx >= 0) {
            shadow = shadow_factor_pos(sidx, world_pos, lp, range);
        }
        lit = lit + lighting.lights[i].color_intensity.rgb
            * (lighting.lights[i].color_intensity.w * falloff * shadow);
    }
    return lit;
}

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) joints: vec4<u32>,
    @location(3) weights: vec4<f32>,
    // Per-vertex, per-instance damage/blood color (second vertex buffer). White =
    // clean; painting reddens + darkens it at the hit location, accumulating.
    @location(4) color: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec3<f32>,
    @location(2) world_pos: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    // Weighted blend of the four influencing joint matrices (LBS).
    let skin =
          in.weights.x * ch.joints[in.joints.x]
        + in.weights.y * ch.joints[in.joints.y]
        + in.weights.z * ch.joints[in.joints.z]
        + in.weights.w * ch.joints[in.joints.w];
    let world = ch.model * skin * vec4<f32>(in.pos, 1.0);
    var out: VsOut;
    out.clip = camera.view_proj * world;
    out.uv = in.uv;
    out.color = in.color;
    out.world_pos = world.xyz;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    // Multiply in the per-vertex blood color (white = unchanged; painted vertices
    // go red/dark, so accumulated shots read as persistent blood on the body), then
    // the local light level + shadow so the character sits in the room's lighting.
    let rgb = c.rgb * in.color * shade_pos(in.world_pos);
    // Opacity 1 (normal) with an alpha-blend target == opaque; <1 fades the whole
    // character out over the death animation (Track A). Textures are opaque (a=1),
    // so the character-wide opacity is the only alpha term.
    return vec4<f32>(rgb, c.a * ch.opacity.x);
}
