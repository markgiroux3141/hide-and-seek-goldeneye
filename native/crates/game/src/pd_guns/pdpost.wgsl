// PD's framebuffer effects (`bondview.c`), done as full-screen passes over a copy
// of the frame. PD redraws each screen line from a framebuffer with
// `bview_copy_pixels` (a textured rectangle, zoomed horizontally about the
// centre by `scale`, times the env colour); here each pixel works out which
// line it is on and samples the copy the same way. Colour maths happens in
// display (gamma) space like the N64's, the copy being an sRGB texture.
//
// mode 0: `bview_draw_slayer_rocket_interlace` — scale 2 - sin(angle), angle
//         30°..150° down the view; env (1,1,0) / (1,1,0.75) in 8-line bands
//         scrolling with `offset`; replaces the frame.
// mode 1: `bview_draw_static` — rows of noise × env (0x4f,0xff,0xff), alpha.
// mode 2: `bview_draw_zoom_blur` — the last frame zoomed by (sx, sy) about
//         the centre, blended at alpha.
// mode 3: `player_draw_fade` — a flat colour (zoom.rgb) at alpha.

struct Post {
    params: vec4<f32>, // mode, lines (240), offset / seed, alpha
    zoom: vec4<f32>,   // sx, sy, _, _
};

@group(0) @binding(0) var<uniform> post: Post;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VOut {
    var o: VOut;
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    o.clip = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    o.uv = vec2<f32>(x, y);
    return o;
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

fn hash(p: vec2<f32>) -> f32 {
    let q = fract(p * vec2<f32>(0.1031, 0.1030));
    let r = q + dot(q, q.yx + 33.33);
    return fract((r.x + r.y) * r.x);
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let mode = u32(post.params.x);
    let lines = post.params.y;
    let line = floor(in.uv.y * lines);
    if (mode == 0u) {
        let angle = radians(30.0) + (radians(150.0) - radians(30.0)) * line / lines;
        let scale = 2.0 - sin(angle);
        let u = 0.5 + (in.uv.x - 0.5) / scale;
        let v = (line + 0.5) / lines;
        let texel = linear_to_srgb(textureSample(src, samp, vec2<f32>(u, v)).rgb);
        let offsety = line - post.params.z;
        let band = offsety - 16.0 * floor(offsety / 16.0);
        var env = vec3<f32>(1.0, 1.0, 0.0);
        if (band >= 8.0) {
            env = vec3<f32>(1.0, 1.0, 191.0 / 255.0);
        }
        return vec4<f32>(srgb_to_linear(texel * env), 1.0);
    }
    if (mode == 1u) {
        // A random source row per line (PD reads random RDRAM as I8).
        let row = floor(hash(vec2<f32>(line, post.params.z)) * 4096.0);
        let x = floor(in.uv.x * 320.0);
        let n = hash(vec2<f32>(x + row * 0.37, row));
        let env = vec3<f32>(79.0 / 255.0, 1.0, 1.0);
        return vec4<f32>(srgb_to_linear(vec3<f32>(n) * env), post.params.w);
    }
    if (mode == 3u) {
        return vec4<f32>(srgb_to_linear(post.zoom.rgb), post.params.w);
    }
    // Zoom blur.
    let uv = vec2<f32>(0.5) + (in.uv - vec2<f32>(0.5)) / post.zoom.xy;
    let texel = textureSample(src, samp, uv).rgb;
    return vec4<f32>(texel, post.params.w);
}
