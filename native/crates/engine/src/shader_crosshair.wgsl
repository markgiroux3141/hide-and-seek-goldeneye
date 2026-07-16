// Screen-space crosshair: a textured, alpha-blended quad (the GoldenEye red
// reticle from `assets/hud/crosshairs.png`). Generated from the vertex index (no
// vertex buffer). Aspect-corrected so it stays square on any window, and offset
// by the free-aim uniform so it floats off center. Drawn last, depth disabled.

struct Overlay {
    // x = height/width (aspect correction); (offset_x, offset_y) = the crosshair's
    // screen-space NDC position (GoldenEye free-aim floats it off center).
    aspect_fix: f32,
    offset_x: f32,
    offset_y: f32,
    _pad: f32,
};
@group(0) @binding(0) var<uniform> overlay: Overlay;

@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

// NDC half-height of the reticle (≈ the JS 112 px on a ~900 px window).
const HALF: f32 = 0.11;

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
    var out: VsOut;
    // Square on screen (aspect-correct x), then float by the free-aim offset.
    let x = c.x * HALF * overlay.aspect_fix + overlay.offset_x;
    let y = c.y * HALF + overlay.offset_y;
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    // Corner [-1,1] → UV [0,1], V flipped (image row 0 = top).
    out.uv = vec2<f32>(c.x * 0.5 + 0.5, 1.0 - (c.y * 0.5 + 0.5));
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv);
}
