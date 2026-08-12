// Placeable props (crate / barrel / furniture): a static textured mesh placed in
// the world by a clip matrix (view_proj · world) and multiplied by a per-instance
// tint. Unlit textured — same N64 look as the viewmodel/character shaders. The tint
// is white by default; the destructible "darken when shot" drives it toward black.

struct Prop {
    view_proj: mat4x4<f32>,
    // Model→world (metres). Needed so the fragment stage can light in world space;
    // `view_proj` is the combined clip matrix and can't be inverted cheaply here.
    world: mat4x4<f32>,
    tint: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: Prop;

@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;
// Emissive/env slot (bound to 1×1 black for props — they aren't metallic). Present
// only so props can reuse the viewmodel texture bind-group layout unchanged.
@group(1) @binding(2) var env_tex: texture_2d<f32>;

// ── Lighting (group 2): the same uniform + shadow cube-array the region shaders
// use, so props sit in the level's lighting and receive its point-light shadows.
// Props RECEIVE light + shadow only — they are not rendered into the shadow cubes,
// so they don't cast (see the renderer's shadow pass).
struct Light {
    pos_range: vec4<f32>,        // xyz = world pos (m), w = range (m)
    color_intensity: vec4<f32>,  // rgb = colour, w = intensity
    params: vec4<f32>,           // x = shadow cube index (< 0 = no shadow)
};
struct Lighting {
    ambient: vec4<f32>,          // rgb = ambient × level, w = flat flag (1 = flat)
    count: vec4<u32>,            // x = active light count
    lights: array<Light, 32>,
};
@group(2) @binding(0) var<uniform> lighting: Lighting;
@group(2) @binding(1) var shadow_maps: texture_cube_array<f32>;
@group(2) @binding(2) var shadow_samp: sampler;

// Identical to the region shaders' shadow test (back-face / second-depth cubes, so
// only a small normal offset + slope bias is needed). See shader_textured.wgsl for
// the full rationale.
fn shadow_factor(idx: i32, frag: vec3<f32>, lp: vec3<f32>, range: f32, n: vec3<f32>, ndl: f32) -> f32 {
    let slope = clamp(1.0 - ndl, 0.0, 1.0);
    let dist_m = length(frag - lp);
    let texel = 2.0 * dist_m / 512.0;
    let offset = frag + n * texel * (0.5 + 0.5 * slope);
    let to_frag = offset - lp;
    let dist = length(to_frag) / max(range, 0.0001);
    let dir = normalize(to_frag);
    let bias = 0.002 + 0.008 * slope;
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

// Same shading multiplier the walls use. Flat mode (ambient.w == 1) keeps the legacy
// look so props match the editor's flat/grid view.
fn shade(world_pos: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    if (lighting.ambient.w > 0.5) {
        let l = normalize(vec3<f32>(0.4, 1.0, 0.6));
        let ndl = abs(dot(n, l));
        return vec3<f32>(0.25 + 0.75 * ndl);
    }
    var lit = lighting.ambient.rgb;
    let count = lighting.count.x;
    for (var i = 0u; i < count; i = i + 1u) {
        let lp = lighting.lights[i].pos_range.xyz;
        let range = max(lighting.lights[i].pos_range.w, 0.0001);
        let d = lp - world_pos;
        let dist = length(d);
        let ldir = d / max(dist, 0.0001);
        let ndl = abs(dot(n, ldir));
        let a = clamp(1.0 - dist / range, 0.0, 1.0);
        let falloff = a * a;
        var shadow = 1.0;
        let sidx = i32(lighting.lights[i].params.x);
        if (sidx >= 0) {
            shadow = shadow_factor(sidx, world_pos, lp, range, n, ndl);
        }
        lit = lit + lighting.lights[i].color_intensity.rgb
            * (lighting.lights[i].color_intensity.w * ndl * falloff * shadow);
    }
    return lit;
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) world_pos: vec3<f32>,
    @location(3) normal: vec3<f32>,
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
    let wp = u.world * vec4<f32>(pos, 1.0);
    out.world_pos = wp.xyz;
    // Props are placed with near-uniform scale, so the world 3×3 is an adequate
    // normal transform (no inverse-transpose needed).
    out.normal = (u.world * vec4<f32>(normal, 0.0)).xyz;
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
    // Receive the level's lighting + point-light shadows, exactly like the walls.
    let lit = shade(in.world_pos, normalize(in.normal));
    return vec4<f32>(base.rgb * u.tint.rgb * lit, base.a * u.tint.a);
}
