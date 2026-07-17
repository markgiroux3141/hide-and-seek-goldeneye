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
// Environment/reflection map for GoldenEye metallic guns (gold/silver/chrome).
// Their base color is mostly BLACK — the metal was meant to be filled by an
// environment reflection. We fake that reflection matcap-style: sample this
// texture by the surface normal (below) and add it, turning the black metal
// gold/silver. Bound to a 1×1 black texture for non-metallic meshes → no-op.
@group(1) @binding(2) var env_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) normal: vec3<f32>,
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
    out.normal = normal;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Palette texel × per-vertex color (the GoldenEye N64 shading): a white
    // palette entry tinted by the vertex color gives the real (e.g. dark metal)
    // surface color. Region meshes carry white vertex colors → no-op.
    let base = textureSample(tex, samp, in.uv) * in.color;
    // Matcap environment reflection: map the surface normal's XY to [0,1] and look
    // up the reflection map. For metallic guns this paints uniform gold/silver over
    // the (black) metal; for everything else `env_tex` is 1×1 black → adds nothing.
    let n = normalize(in.normal);
    let mcap_uv = n.xy * 0.5 + vec2<f32>(0.5, 0.5);
    // 1.6× so the metal reads as bright polished gold/silver (the reflection map is
    // a mid-tone; the black base contributes ~nothing). Tune this to taste — it's
    // the metallic-gun brightness knob. Highlights clamp to white on their own.
    let refl = textureSample(env_tex, samp, mcap_uv).rgb * 1.6;
    return vec4<f32>(base.rgb + refl, base.a);
}
