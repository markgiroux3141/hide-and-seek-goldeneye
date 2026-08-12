// Grid (checkerboard) region shader: transform by camera view-proj, then shade via
// the shared `shade()` — either the legacy fixed-directional look (flat flag set) or
// the authored point lights + ambient (real lighting). Same lighting model as
// shader_textured.wgsl; this one draws a procedural world-space checker as albedo.

struct Camera {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

// ── Scene lighting (point lights + ambient + flat/real flag). Shared shape with
// shader_textured.wgsl; only the bind-group index differs (grid has no material
// group, so lighting sits at group(1) here, group(2) there).
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
@group(1) @binding(0) var<uniform> lighting: Lighting;
@group(1) @binding(1) var shadow_maps: texture_cube_array<f32>;
@group(1) @binding(2) var shadow_samp: sampler;

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
    // UV is present in the shared TexVertex layout but unused in grid view.
    @location(2) uv: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    // World-space position (the mesh is already in world/meter space — no model
    // matrix — so the vertex position doubles as world position). Used for the
    // checkerboard, which needs stable world coords, not screen coords.
    @location(1) world_pos: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(in.pos, 1.0);
    out.normal = in.normal;
    out.world_pos = in.pos;
    return out;
}

// One world-tile (WT) in meters — the authoring grid unit (WORLD_SCALE). A
// checker cell = 1 WT, so each stair step / brush step reads as one square.
const CHECK_CELL: f32 = 0.25;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // 3D checkerboard in world space: on any axis-aligned face the constant axis
    // drops out, leaving a 2D checker. A tiny bias keeps faces sitting exactly on
    // a cell boundary (e.g. the y=0 floor) from parity-flickering.
    let cell = floor((in.world_pos + vec3<f32>(0.0005)) / CHECK_CELL);
    let parity = (i32(cell.x) + i32(cell.y) + i32(cell.z)) & 1;
    let base = select(vec3<f32>(0.72, 0.74, 0.82), vec3<f32>(0.44, 0.46, 0.54), parity == 1);

    let lit = base * shade(in.world_pos, normalize(in.normal));
    return vec4<f32>(lit, 1.0);
}
