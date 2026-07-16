// Screen-space HUD text: textured quads sampling a code-defined glyph atlas
// (white glyphs on a transparent background). Positions arrive already in NDC, so
// there's no camera/projection. Drawn in the depth-cleared overlay pass, on top of
// everything, alpha-blended. The atlas alpha keys the glyph pixels; background
// texels (alpha 0) are discarded so nothing but the digits shows.

@group(0) @binding(0) var atlas: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.pos = vec4<f32>(in.pos, 0.0, 1.0);
    out.uv = in.uv;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(atlas, samp, in.uv);
    if (c.a < 0.5) {
        discard;
    }
    return c;
}
