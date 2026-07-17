// Full-screen screen-space overlay: a fullscreen quad that samples a texture and
// multiplies by a tint (rgba). Used for the radial health HUD (health RGBA × a
// white/opacity tint), the red damage flash (a 1×1 white texture × red/alpha),
// and the death dimmer (white × black/0.85). Alpha-blended, no depth.

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct Tint { color: vec4<f32> };
@group(1) @binding(0) var<uniform> tint: Tint;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var p = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
    );
    let xy = p[vi];
    var o: VsOut;
    o.clip = vec4<f32>(xy, 0.0, 1.0);
    // NDC [-1,1] → UV [0,1], with v flipped (image row 0 = visual top).
    o.uv = vec2<f32>((xy.x + 1.0) * 0.5, 1.0 - (xy.y + 1.0) * 0.5);
    return o;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv) * tint.color;
}
