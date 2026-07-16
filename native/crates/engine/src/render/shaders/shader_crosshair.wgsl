// Screen-space crosshair: an alpha-blended quad generated from the vertex index
// (no vertex buffer). Aspect-corrected so it stays square on any window, and
// offset by the free-aim uniform so it floats off center. Drawn last, depth
// disabled. Two styles, chosen by `mode`:
//   mode 0 — the GoldenEye reticle textured from `assets/hud/crosshairs.png`
//            (HUNT free-aim; floated off center).
//   mode 1 — a small procedural white cross (BUILD editor pick cursor), centered.

struct Overlay {
    // x = height/width (aspect correction); (offset_x, offset_y) = the crosshair's
    // screen-space NDC position (GoldenEye free-aim floats it off center).
    aspect_fix: f32,
    offset_x: f32,
    offset_y: f32,
    // 0 = textured reticle (HUNT), 1 = procedural white cross (BUILD).
    mode: f32,
};
@group(0) @binding(0) var<uniform> overlay: Overlay;

@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

// NDC half-height of the textured reticle (≈ the JS 112 px on a ~900 px window).
const HALF: f32 = 0.11;
// NDC half-height of the small BUILD cross (much smaller than the reticle).
const BUILD_HALF: f32 = 0.032;
// BUILD cross geometry, in quad-UV units from the centre (0.5, 0.5):
// half-thickness of each arm, and how far the arms reach (with a tiny centre gap).
const CROSS_THICK: f32 = 0.11;
const CROSS_REACH: f32 = 0.5;
const CROSS_GAP: f32 = 0.12;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Unit quad corners (two tris).
    var quad = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
    );
    let c = quad[vi];
    let half = select(HALF, BUILD_HALF, overlay.mode > 0.5);
    var out: VsOut;
    // Square on screen (aspect-correct x), then float by the free-aim offset.
    let x = c.x * half * overlay.aspect_fix + overlay.offset_x;
    let y = c.y * half + overlay.offset_y;
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    // Corner [-1,1] → UV [0,1], V flipped (image row 0 = top).
    out.uv = vec2<f32>(c.x * 0.5 + 0.5, 1.0 - (c.y * 0.5 + 0.5));
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (overlay.mode > 0.5) {
        // BUILD: a small white plus/cross drawn procedurally in the quad. `d` is
        // the per-axis distance from centre in UV units.
        let d = abs(in.uv - vec2<f32>(0.5, 0.5));
        let horiz = d.y < CROSS_THICK && d.x < CROSS_REACH && d.x > CROSS_GAP;
        let vert = d.x < CROSS_THICK && d.y < CROSS_REACH && d.y > CROSS_GAP;
        if (horiz || vert) {
            return vec4<f32>(1.0, 1.0, 1.0, 1.0);
        }
        return vec4<f32>(0.0, 0.0, 0.0, 0.0); // transparent (alpha-blended)
    }
    return textureSample(tex, samp, in.uv);
}
