//! A software stand-in for the RSP + RDP, just big enough for PD's menus: a
//! 320×220 framebuffer (`FBALLOC_WIDTH_LO` × `FBALLOC_HEIGHT_LO`,
//! constants.h:3654), the scissor, `gDPFillRectangle`, shaded / textured
//! triangles through PD's ortho and holoray matrices, texture rectangles, and
//! the `G_RM_XLU_SURF` blend (`src·a + dst·(1 − a)`).
//!
//! **Substitution:** the RDP's edge walker, coverage and 5551 framebuffer are
//! not emulated. Triangles are sampled at pixel centres with a top-left rule,
//! colour is kept in f32, and `G_RM_AA_*` modes blend like their non-AA twins.
//! The framebuffer can be quantised to RGBA5551 on output (the VI's input).
//!
//! Coordinates: PD's ortho modelview (`ortho_configure_mtx`, savebuffer.c:48)
//! is `x·0.1 + (0.5 − w)/2`, `−y·0.1 + (0.5 + h)/2` against a 10-unit near
//! plane, so a UI vertex `(10x, 10y, −10)` lands at pixel `(x + ¼, y − ¼)`.
//! [`Gfx::ortho`] and [`Gfx::holoray`] are those two matrices.

use image::GenericImageView;

/// One texture: RGBA 0..1, with PD's tile addressing.
#[derive(Clone)]
pub struct Texture {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[f32; 4]>,
    pub wrap_s: Addr,
    pub wrap_t: Addr,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Addr {
    Wrap,
    Clamp,
    Mirror,
}

impl Texture {
    pub fn load_png(path: &std::path::Path, wrap_s: Addr, wrap_t: Addr) -> Result<Texture, String> {
        let img = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (w, h) = img.dimensions();
        let rgba = img.to_rgba8();
        let px = rgba.pixels().map(|p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0, p[3] as f32 / 255.0]).collect();
        Ok(Texture { w: w as usize, h: h as usize, px, wrap_s, wrap_t })
    }

    pub fn from_rgba(w: usize, h: usize, px: Vec<[f32; 4]>, wrap_s: Addr, wrap_t: Addr) -> Texture {
        Texture { w, h, px, wrap_s, wrap_t }
    }

    fn addr(i: i32, n: usize, mode: Addr) -> usize {
        let n = n as i32;
        match mode {
            Addr::Clamp => i.clamp(0, n - 1) as usize,
            Addr::Wrap => i.rem_euclid(n) as usize,
            Addr::Mirror => {
                let m = i.rem_euclid(2 * n);
                (if m >= n { 2 * n - 1 - m } else { m }) as usize
            }
        }
    }

    pub fn texel(&self, s: i32, t: i32) -> [f32; 4] {
        let x = Self::addr(s, self.w, self.wrap_s);
        let y = Self::addr(t, self.h, self.wrap_t);
        self.px[y * self.w + x]
    }

    /// `G_TF_POINT`.
    pub fn point(&self, s: f32, t: f32) -> [f32; 4] {
        self.texel(s.floor() as i32, t.floor() as i32)
    }

    /// `G_TF_BILERP`: the RDP's three-point filter (the triangle of the three
    /// texels nearest the sample, not a four-texel bilinear).
    pub fn bilerp(&self, s: f32, t: f32) -> [f32; 4] {
        let (s, t) = (s - 0.5, t - 0.5);
        let (s0, t0) = (s.floor(), t.floor());
        let (fs, ft) = (s - s0, t - t0);
        let (s0, t0) = (s0 as i32, t0 as i32);
        let t00 = self.texel(s0, t0);
        let t10 = self.texel(s0 + 1, t0);
        let t01 = self.texel(s0, t0 + 1);
        let t11 = self.texel(s0 + 1, t0 + 1);
        let mut out = [0.0; 4];
        if fs + ft < 1.0 {
            for i in 0..4 {
                out[i] = t00[i] + fs * (t10[i] - t00[i]) + ft * (t01[i] - t00[i]);
            }
        } else {
            for i in 0..4 {
                out[i] = t11[i] + (1.0 - fs) * (t01[i] - t11[i]) + (1.0 - ft) * (t10[i] - t11[i]);
            }
        }
        out
    }
}

/// A colour word `0xRRGGBBAA` → RGBA 0..1.
pub fn rgba(c: u32) -> [f32; 4] {
    [((c >> 24) & 0xff) as f32 / 255.0, ((c >> 16) & 0xff) as f32 / 255.0, ((c >> 8) & 0xff) as f32 / 255.0, (c & 0xff) as f32 / 255.0]
}

/// The colour combiner modes the menus use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cc {
    /// `G_CC_SHADE`.
    Shade,
    /// `G_CC_MODULATEI`: rgb = TEXEL0 × SHADE, a = SHADE.
    ModulateI,
    /// `G_CC_MODULATEIA`: rgba = TEXEL0 × SHADE.
    ModulateIA,
    /// `TEXEL0 × ENVIRONMENT` for rgb and alpha.
    TexEnv,
    /// `G_CC_DECALRGBA`: TEXEL0.
    Decal,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    Point,
    Bilerp,
}

/// Blender: `G_RM_XLU_SURF` (and its AA twin) or opaque.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Blend {
    Xlu,
    Opaque,
}

/// A transformed vertex: screen position, `1/w`, texture coords in texels, shade.
#[derive(Clone, Copy, Debug, Default)]
pub struct SV {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub inv_w: f32,
    pub s: f32,
    pub t: f32,
    pub c: [f32; 4],
}

pub struct TriState<'a> {
    pub cc: Cc,
    pub tex: Option<&'a Texture>,
    pub filter: Filter,
    pub blend: Blend,
    pub env: [f32; 4],
    /// Perspective-correct texture coordinates (`G_TP_PERSP`).
    pub persp: bool,
    /// Z-buffered (`G_ZBUFFER` + a `ZB_` render mode): test and update.
    pub zbuf: bool,
    /// `G_CULL_BACK`.
    pub cull_back: bool,
}

pub struct Gfx {
    pub w: usize,
    pub h: usize,
    /// Premultiplied RGBA. The frame itself is opaque (alpha 1); a layer
    /// ([`Gfx::push_layer`]) starts transparent and is composited back with
    /// "over", which is exactly the sequential XLU blend it stands in for.
    pub fb: Vec<[f32; 4]>,
    pub zb: Vec<f32>,
    /// `gDPSetScissor`: x1, y1, x2, y2 (exclusive), in framebuffer pixels.
    pub scissor: [i32; 4],
    /// `g_UiScaleX`: 2 in hi-res.
    pub uiscale: i32,
    layers: Vec<Vec<[f32; 4]>>,
    pending: Option<Vec<[f32; 4]>>,
}

impl Gfx {
    pub fn new(w: usize, h: usize) -> Gfx {
        Gfx { w, h, fb: vec![[0.0, 0.0, 0.0, 1.0]; w * h], zb: vec![f32::INFINITY; w * h], scissor: [0, 0, w as i32, h as i32], uiscale: 1, layers: Vec::new(), pending: None }
    }

    pub fn clear(&mut self, c: [f32; 3]) {
        self.fb.fill([c[0], c[1], c[2], 1.0]);
        self.zb.fill(f32::INFINITY);
    }

    /// Draw into a fresh transparent layer until [`Gfx::pop_layer`].
    pub fn push_layer(&mut self) {
        let fresh = vec![[0.0; 4]; self.w * self.h];
        let base = std::mem::replace(&mut self.fb, fresh);
        self.layers.push(base);
    }

    /// Back to the frame underneath, keeping the layer aside: draw what PD's
    /// display list orders *before* the layer's contents, then
    /// [`Gfx::composite_pending`].
    pub fn swap_to_base(&mut self) {
        let Some(base) = self.layers.pop() else { return };
        let layer = std::mem::replace(&mut self.fb, base);
        self.pending = Some(layer);
    }

    /// Composite the layer set aside by [`Gfx::swap_to_base`] over the frame.
    pub fn composite_pending(&mut self) {
        let Some(layer) = self.pending.take() else { return };
        for (d, s) in self.fb.iter_mut().zip(layer.iter()) {
            let ia = 1.0 - s[3];
            for i in 0..4 {
                d[i] = s[i] + d[i] * ia;
            }
        }
    }

    pub fn clear_z(&mut self) {
        self.zb.fill(f32::INFINITY);
    }

    pub fn set_scissor(&mut self, x1: i32, y1: i32, x2: i32, y2: i32) {
        let (w, h) = (self.w as i32, self.h as i32);
        self.scissor = [x1.clamp(0, w), y1.clamp(0, h), x2.clamp(0, w), y2.clamp(0, h)];
    }

    pub fn full_scissor(&mut self) {
        self.scissor = [0, 0, self.w as i32, self.h as i32];
    }

    #[inline]
    pub fn blend_px(&mut self, x: i32, y: i32, c: [f32; 4], mode: Blend) {
        let [sx1, sy1, sx2, sy2] = self.scissor;
        if x < sx1 || y < sy1 || x >= sx2 || y >= sy2 {
            return;
        }
        let p = &mut self.fb[y as usize * self.w + x as usize];
        match mode {
            Blend::Opaque => {
                *p = [c[0].clamp(0.0, 1.0), c[1].clamp(0.0, 1.0), c[2].clamp(0.0, 1.0), 1.0];
            }
            Blend::Xlu => {
                let a = c[3].clamp(0.0, 1.0);
                if a <= 0.0 {
                    return;
                }
                for i in 0..3 {
                    p[i] = c[i].clamp(0.0, 1.0) * a + p[i] * (1.0 - a);
                }
                p[3] = a + p[3] * (1.0 - a);
            }
        }
    }

    /// `gDPFillRectangle` in 1-cycle mode with `G_CC_PRIMITIVE` + XLU
    /// (`text_begin_boxmode`, text.c:414): lower-right exclusive.
    pub fn fill_rect(&mut self, x1: i32, y1: i32, x2: i32, y2: i32, colour: u32) {
        let c = rgba(colour);
        for y in y1..y2 {
            for x in x1..x2 {
                self.blend_px(x, y, c, Blend::Xlu);
            }
        }
    }

    /// `gDPFillRectangleScaled` (gbiex.h): x multiplied by `g_UiScaleX`.
    pub fn fill_rect_scaled(&mut self, x1: i32, y1: i32, x2: i32, y2: i32, colour: u32) {
        let s = self.uiscale;
        self.fill_rect(x1 * s, y1, x2 * s, y2, colour);
    }

    /// A UI vertex through the ortho matrix (`ortho_begin`): PD passes pixel × 10.
    pub fn ortho(&self, x10: f32, y10: f32) -> (f32, f32) {
        (x10 * 0.1 * self.uiscale as f32 + 0.25, y10 * 0.1 - 0.25)
    }

    /// A vertex through `ortho_configure_full_mtx` + the ortho frustum (near 10):
    /// the holoray planes, whose back edge sits at `z = −10 − a1`. Returns
    /// (screen x, screen y, 1/w).
    pub fn holoray(&self, x: f32, y: f32, z: f32) -> (f32, f32, f32) {
        let (w, h) = (self.w as f32, self.h as f32);
        let xe = x * self.uiscale as f32 + (0.5 - w) / 2.0;
        let ye = -y + (0.5 + h) / 2.0;
        let inv = 10.0 / -z;
        (w / 2.0 + xe * inv, h / 2.0 - ye * inv, 1.0 / -z)
    }

    /// Rasterise one triangle.
    pub fn tri(&mut self, v: [SV; 3], st: &TriState) {
        let area = (v[1].x - v[0].x) * (v[2].y - v[0].y) - (v[2].x - v[0].x) * (v[1].y - v[0].y);
        if area.abs() < 1e-9 {
            return;
        }
        if st.cull_back && area > 0.0 {
            // Screen y points down, so a front face (CCW in PD's y-up clip
            // space) has negative area here.
            return;
        }
        let [sx1, sy1, sx2, sy2] = self.scissor;
        let minx = v.iter().map(|p| p.x).fold(f32::INFINITY, f32::min).floor().max(sx1 as f32) as i32;
        let maxx = v.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max).ceil().min(sx2 as f32) as i32;
        let miny = v.iter().map(|p| p.y).fold(f32::INFINITY, f32::min).floor().max(sy1 as f32) as i32;
        let maxy = v.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max).ceil().min(sy2 as f32) as i32;
        if minx >= maxx || miny >= maxy {
            return;
        }
        // Edge functions with a top-left fill rule.
        let edge = |a: &SV, b: &SV, px: f32, py: f32| (b.x - a.x) * (py - a.y) - (b.y - a.y) * (px - a.x);
        let sign = area.signum();
        let is_tl = |a: &SV, b: &SV| {
            let (dx, dy) = ((b.x - a.x) * sign, (b.y - a.y) * sign);
            (dy == 0.0 && dx > 0.0) || dy < 0.0
        };
        let tl = [is_tl(&v[1], &v[2]), is_tl(&v[2], &v[0]), is_tl(&v[0], &v[1])];
        let inv_area = 1.0 / area;
        for py in miny..maxy {
            let fy = py as f32 + 0.5;
            for px in minx..maxx {
                let fx = px as f32 + 0.5;
                let w0 = edge(&v[1], &v[2], fx, fy) * inv_area;
                let w1 = edge(&v[2], &v[0], fx, fy) * inv_area;
                let w2 = edge(&v[0], &v[1], fx, fy) * inv_area;
                let inside = |w: f32, tl: bool| w > 0.0 || (w == 0.0 && tl);
                if !(inside(w0, tl[0]) && inside(w1, tl[1]) && inside(w2, tl[2])) {
                    continue;
                }
                let idx = py as usize * self.w + px as usize;
                if st.zbuf {
                    let z = w0 * v[0].z + w1 * v[1].z + w2 * v[2].z;
                    if z > self.zb[idx] {
                        continue;
                    }
                    self.zb[idx] = z;
                }
                let mut shade = [0.0f32; 4];
                for i in 0..4 {
                    shade[i] = w0 * v[0].c[i] + w1 * v[1].c[i] + w2 * v[2].c[i];
                }
                let tex = st.tex.map(|t| {
                    let (s, tt) = if st.persp {
                        let iw = w0 * v[0].inv_w + w1 * v[1].inv_w + w2 * v[2].inv_w;
                        let s = (w0 * v[0].s * v[0].inv_w + w1 * v[1].s * v[1].inv_w + w2 * v[2].s * v[2].inv_w) / iw;
                        let tt = (w0 * v[0].t * v[0].inv_w + w1 * v[1].t * v[1].inv_w + w2 * v[2].t * v[2].inv_w) / iw;
                        (s, tt)
                    } else {
                        (w0 * v[0].s + w1 * v[1].s + w2 * v[2].s, w0 * v[0].t + w1 * v[1].t + w2 * v[2].t)
                    };
                    match st.filter {
                        Filter::Point => t.point(s, tt),
                        Filter::Bilerp => t.bilerp(s, tt),
                    }
                });
                let c = combine(st.cc, tex.unwrap_or([1.0; 4]), shade, st.env);
                self.blend_px(px, py, c, st.blend);
            }
        }
    }

    /// Two triangles `0,1,2` and `2,3,0` (`gSPTri2(0, 1, 2, 2, 3, 0)`).
    pub fn quad(&mut self, v: [SV; 4], st: &TriState) {
        self.tri([v[0], v[1], v[2]], st);
        self.tri([v[2], v[3], v[0]], st);
    }

    /// `gSPTextureRectangle` in 1-cycle mode, coordinates in pixels (not
    /// 10.2), `s0/t0` in texels, `dsdx/dtdy` in texels per pixel.
    pub fn tex_rect(&mut self, x1: i32, y1: i32, x2: i32, y2: i32, tex: &Texture, s0: f32, t0: f32, dsdx: f32, dtdy: f32, cc: Cc, env: [f32; 4], filter: Filter) {
        for y in y1..y2 {
            for x in x1..x2 {
                let s = s0 + (x - x1) as f32 * dsdx;
                let t = t0 + (y - y1) as f32 * dtdy;
                let texel = match filter {
                    Filter::Point => tex.point(s, t),
                    Filter::Bilerp => tex.bilerp(s + 0.5, t + 0.5),
                };
                let c = combine(cc, texel, [1.0; 4], env);
                self.blend_px(x, y, c, Blend::Xlu);
            }
        }
    }

    /// The framebuffer as RGBA8, optionally quantised to RGBA5551 like the N64's.
    pub fn rgba8(&self, n64: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.w * self.h * 4);
        for p in &self.fb {
            for c in &p[..3] {
                let mut v = (c.clamp(0.0, 1.0) * 255.0).round() as u32;
                if n64 {
                    v = (v >> 3) << 3;
                    v |= v >> 5;
                }
                out.push(v as u8);
            }
            out.push(255);
        }
        out
    }
}

pub fn combine(cc: Cc, t: [f32; 4], shade: [f32; 4], env: [f32; 4]) -> [f32; 4] {
    match cc {
        Cc::Shade => shade,
        Cc::ModulateI => [t[0] * shade[0], t[1] * shade[1], t[2] * shade[2], shade[3]],
        Cc::ModulateIA => [t[0] * shade[0], t[1] * shade[1], t[2] * shade[2], t[3] * shade[3]],
        Cc::TexEnv => [t[0] * env[0], t[1] * env[1], t[2] * env[2], t[3] * env[3]],
        Cc::Decal => t,
    }
}
