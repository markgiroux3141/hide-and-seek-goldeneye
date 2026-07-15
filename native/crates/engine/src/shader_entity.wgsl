// Entity shader: opaque, solid red with cheap Lambert shading so dynamic props
// (the hunter) read as clearly distinct from the gray level geometry. Reuses the
// camera uniform (group 0) and the standard Vertex layout.

struct Camera {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(in.pos, 1.0);
    out.normal = in.normal;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let l = normalize(vec3<f32>(0.4, 1.0, 0.6));
    let ndl = max(dot(normalize(in.normal), l), 0.0);
    let base = vec3<f32>(0.85, 0.12, 0.12); // hunter red
    return vec4<f32>(base * (0.35 + 0.65 * ndl), 1.0);
}
