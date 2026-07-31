// Placeable props (crate / barrel / furniture): a static textured mesh placed in
// the world by a clip matrix (view_proj · world) and multiplied by a per-instance
// tint. Unlit textured — same N64 look as the viewmodel/character shaders. The tint
// is white by default; the destructible "darken when shot" drives it toward black.

struct Prop {
    view_proj: mat4x4<f32>,
    tint: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: Prop;

@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;
// Emissive/env slot (bound to 1×1 black for props — they aren't metallic). Present
// only so props can reuse the viewmodel texture bind-group layout unchanged.
@group(1) @binding(2) var env_tex: texture_2d<f32>;

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
    out.pos = u.view_proj * vec4<f32>(pos, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let texel = textureSample(tex, samp, in.uv);
    // The prop pipeline alpha-blends, so a texel's alpha is its real opacity. Props
    // carry two kinds of transparency, both handled by that blend:
    //   • cutout (chain-link, grates): alpha is 0 or 1 — the 0s must vanish;
    //   • translucent (GoldenEye "secondary" glass/screens): alpha ≈ 0.5-ish — must
    //     show what's behind.
    // Only *fully* transparent texels are discarded (culls cutout holes + saves the
    // blend); everything else keeps its alpha and blends. Opaque props (alpha 1) are
    // untouched.
    if texel.a < 0.02 {
        discard;
    }
    // Texel × baked vertex color × per-instance tint. Tint white = untouched; a
    // darkened tint dims the whole prop (the shot-damage feedback in Milestone 3).
    let base = texel * in.color;
    return vec4<f32>(base.rgb * u.tint.rgb, base.a * u.tint.a);
}
