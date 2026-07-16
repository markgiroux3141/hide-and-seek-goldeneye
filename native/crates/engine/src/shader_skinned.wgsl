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

const MAX_JOINTS: u32 = 16u;
struct Char {
    // World placement of the whole character (GE-scale → metres + position).
    model: mat4x4<f32>,
    // Skinning matrices: global(joint) · inverseBind(joint). Bind pose = identity.
    joints: array<mat4x4<f32>, MAX_JOINTS>,
};
@group(2) @binding(0) var<uniform> ch: Char;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) joints: vec4<u32>,
    @location(3) weights: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
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
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    return vec4<f32>(c.rgb, 1.0);
}
