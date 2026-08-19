// Textured region/structure shader: sample a per-zone BMP × a single fixed
// directional light. Two-sided lighting (abs of N·L) because the mesh is drawn
// with culling off and some hand-emitted geometry (stairs, structures) is
// single-winding — this keeps both faces lit rather than one going black.

struct Camera {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct Material {
    // .x  = tile-unit → texture-space scale (JS `texture.repeat`); UVs arrive in WT.
    // .yz = texture-space offset (JS `texture.offset`), applied AFTER the scale so
    //       it slides the texture across the surface in whole-texture units — this
    //       is what lets a theme align a band or a tile grid to a wall.
    // .w  = unused.
    // Packed as a vec4 so the Rust-side uniform (16 bytes) matches the WGSL std140
    // layout exactly (a bare `f32 + vec3` pad would round up to 32).
    params: vec4<f32>,
};
@group(1) @binding(2) var<uniform> material: Material;

// ── Scene lighting (point lights + ambient + flat/real flag). Shared shape with
// shader.wgsl; here it sits at group(2), after the material group.
struct Light {
    pos_range: vec4<f32>,        // xyz = world pos (m), w = range (m)
    color_intensity: vec4<f32>,  // rgb = colour, w = intensity
    params: vec4<f32>,           // x = shadow cube index (< 0 = no shadow)
};
struct Lighting {
    ambient: vec4<f32>,          // rgb = ambient colour × level, w = flat flag (1 = flat)
    count: vec4<u32>,            // x = active light count
    lights: array<Light, 32>,
};
@group(2) @binding(0) var<uniform> lighting: Lighting;
@group(2) @binding(1) var shadow_maps: texture_cube_array<f32>;
@group(2) @binding(2) var shadow_samp: sampler;

// Fraction lit (0 shadowed .. 1 lit) for a fragment against light `idx`'s distance
// cube. Stored + compared as normalised light-distance; a 5-tap PCF around the
// sample direction softens the edge. `textureSampleLevel` (explicit LOD) is used so
// it's valid inside the light loop's control flow.
//
// Acne control is two-part: (1) a NORMAL OFFSET nudges the receiver point off its own
// surface along `n`, sized to ~one shadow texel at this distance and widened at
// grazing angles — this is the primary fix and moves the fragment out of its own
// occluder; (2) a slope-scaled depth `bias` mops up the remainder. A single constant
// bias can't do this because the stored value is normalised light-distance, so the
// world-space slack it buys shrinks with range and vanishes at grazing angles.
fn shadow_factor(idx: i32, frag: vec3<f32>, lp: vec3<f32>, range: f32, n: vec3<f32>, ndl: f32) -> f32 {
    // 0 head-on .. 1 at grazing incidence — how much slack the surface needs.
    let slope = clamp(1.0 - ndl, 0.0, 1.0);
    // Shadow texel size in world metres at this distance (90° face frustum, 512² face).
    let dist_m = length(frag - lp);
    let texel = 2.0 * dist_m / 512.0;
    // Small normal offset only. The shadow pass renders BACK faces (second-depth), so
    // the receiver's own surface isn't in the map and self-shadow acne is already gone
    // — a big grazing offset here is what leaked light across wall seams, so keep it
    // light (a fraction of a texel) purely to keep the PCF taps off the surface.
    let offset = frag + n * texel * (0.5 + 0.5 * slope);
    let to_frag = offset - lp;
    let dist = length(to_frag) / max(range, 0.0001);
    let dir = normalize(to_frag);
    // Slope-scaled depth bias (normalised units) on top of the normal offset.
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

// Surface shading multiplier applied to albedo. `world_pos`/`n` are world/metre
// space. `ambient.w == 1` keeps the legacy fixed-directional look unchanged.
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
        let ndl = abs(dot(n, ldir)); // two-sided, matching the flat look
        let a = clamp(1.0 - dist / range, 0.0, 1.0);
        let falloff = a * a; // smooth window: reaches 0 at `range`
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

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    // World/metre-space position — the region/structure mesh has no model matrix,
    // so the vertex position doubles as world position (feeds the point lights).
    @location(2) world_pos: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(in.pos, 1.0);
    out.normal = in.normal;
    out.uv = in.uv;
    out.world_pos = in.pos;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv * material.params.x + material.params.yz);
    // Alpha-test (JS `alphaTest: 0.5`): cut out the transparent texels of the
    // railing texture. Opaque zone textures decode to alpha 1, so they're
    // unaffected — and discard is order-independent, needing no blend/sort.
    if (c.a < 0.5) {
        discard;
    }
    let lit = c.rgb * shade(in.world_pos, normalize(in.normal));
    return vec4<f32>(lit, 1.0);
}
