// Explosion fireball billboards: camera-facing textured quads built CPU-side in
// world space (so no per-instance basis is needed here), projected by the scene
// camera and drawn additively in the forward pass — depth-tested against the scene
// (occluded by nearer walls) but not depth-writing, so overlapping quads all glow.
// The atlas is pre-coloured (grayscale detail × GoldenEye colour thumbnail), so this
// is a plain sample × per-vertex colour; the vertex colour's alpha carries the
// blast's fade-out.

struct Camera {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

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
    @location(1) normal: vec3<f32>, // unused (quad already faces the camera)
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    out.pos = camera.view_proj * vec4<f32>(pos, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let t = textureSample(tex, samp, in.uv);
    // Pre-coloured atlas × fade. Additive blend (SrcAlpha, One) uses the alpha, so
    // transparent cloud edges add ~nothing and the bright core glows.
    return vec4<f32>(t.rgb * in.color.rgb, t.a * in.color.a);
}
