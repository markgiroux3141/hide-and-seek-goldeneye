// Selection highlight: draws the selected face's quad as a translucent overlay
// so the user can see what push/pull will act on. Reuses the camera uniform
// (group 0) and the standard Vertex layout (pos + normal; normal ignored).

struct Camera {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> @builtin(position) vec4<f32> {
    return camera.view_proj * vec4<f32>(in.pos, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    // Warm yellow, semi-transparent (blended over the wall).
    return vec4<f32>(1.0, 0.85, 0.2, 0.35);
}

// Surface tint: washes the whole surface a tool is acting on, under the yellow
// overlay above. Deliberately a **cool** hue at a much lower alpha — the point is to
// read as "this plane", not to compete with the warm outline drawn on top of it, and a
// complementary hue separates the two even where they overlap.
@fragment
fn fs_tint() -> @location(0) vec4<f32> {
    return vec4<f32>(0.25, 0.65, 1.0, 0.13);
}
