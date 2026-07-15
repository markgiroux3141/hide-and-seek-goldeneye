// Screen-space crosshair: a small `+` of two bars, generated from the vertex
// index (no vertex buffer, no camera). Aspect is corrected via a uniform so the
// bars stay square on any window. Drawn last, depth test disabled.

struct Overlay {
    // x = height/width (aspect correction), yzw unused/padding.
    aspect_fix: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};
@group(0) @binding(0) var<uniform> overlay: Overlay;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // Two bars in NDC: horizontal then vertical, 6 verts each (2 tris).
    let long = 0.020;
    let thick = 0.0025;
    var p = array<vec2<f32>, 12>(
        // horizontal bar
        vec2<f32>(-long, -thick), vec2<f32>( long, -thick), vec2<f32>( long,  thick),
        vec2<f32>(-long, -thick), vec2<f32>( long,  thick), vec2<f32>(-long,  thick),
        // vertical bar
        vec2<f32>(-thick, -long), vec2<f32>( thick, -long), vec2<f32>( thick,  long),
        vec2<f32>(-thick, -long), vec2<f32>( thick,  long), vec2<f32>(-thick,  long),
    );
    var v = p[vi];
    v.x = v.x * overlay.aspect_fix; // keep the cross square
    return vec4<f32>(v, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.95, 0.95, 0.95, 1.0);
}
