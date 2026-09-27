//! PD's HUD text (`text.c`): the ROM fonts (`struct font`, `text_load_font`),
//! `text_measure`, and the two renderers the HUD uses — `text_render_v1`
//! (numbers: glyph core in the text colour with a halo in the glow colour) and
//! `text_render_v2` (the weapon/function names, with the wave highlight) —
//! drawn into a [`Canvas`] of PD screen pixels.
//!
//! Glyphs are CI4, 16 texels a row, `height + 2` rows including the halo
//! border, looked up through IA16 TLUTs (`var8007fb3c`, `var8007fb5c`).

use std::path::Path;

/// One PD-pixel canvas (premultiplied RGBA 0..1), composited over the frame.
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[f32; 4]>,
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Self {
        Canvas { w, h, px: vec![[0.0; 4]; w * h] }
    }

    /// `G_RM_XLU_SURF`: `src·a + dst·(1 − a)`.
    pub fn blend(&mut self, x: i32, y: i32, rgb: [f32; 3], a: f32) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h || a <= 0.0 {
            return;
        }
        let p = &mut self.px[y as usize * self.w + x as usize];
        for i in 0..3 {
            p[i] = rgb[i] * a + p[i] * (1.0 - a);
        }
        p[3] = a + p[3] * (1.0 - a);
    }

    /// `gDPFillRectangle` in 1-cycle mode (`text_begin_boxmode`): the lower
    /// right edge is exclusive.
    pub fn fill_rect(&mut self, x1: i32, y1: i32, x2: i32, y2: i32, colour: u32) {
        let (rgb, a) = split(colour);
        for y in y1..y2 {
            for x in x1..x2 {
                self.blend(x, y, rgb, a);
            }
        }
    }

    /// Premultiplied RGBA8.
    pub fn rgba8(&self) -> Vec<u8> {
        self.px.iter().flat_map(|p| p.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)).collect()
    }
}

pub fn split(c: u32) -> ([f32; 3], f32) {
    let b = |s: u32| ((c >> s) & 0xff) as f32 / 255.0;
    ([b(24), b(16), b(8)], b(0))
}

/// `colour_blend` (`game_006900.c:33`).
pub fn colour_blend(a: u32, b: u32, aweight: u32) -> u32 {
    let bweight = 0xff - aweight;
    let ch = |s: u32| ((aweight * ((a >> s) & 0xff) + bweight * ((b >> s) & 0xff)) >> 8) << s;
    ch(24) | ch(16) | ch(8) | ch(0)
}

/// `struct fontchar`.
#[derive(Clone, Copy, Debug, Default)]
pub struct FontChar {
    pub index: u8,
    pub baseline: i32,
    pub height: i32,
    pub width: i32,
    pub kerningindex: i32,
    pixeldata: usize,
}

/// `struct font` + its pixel data.
pub struct Font {
    pub kerning: Vec<i32>,
    pub chars: Vec<FontChar>,
    data: Vec<u8>,
}

/// `var8007fb3c`: the v2 TLUT (alpha bytes; intensity is always 0xff).
const TLUT_V2: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0x24, 0x48, 0x6c, 0x90, 0xb4, 0xd8, 0xff];
/// `var8007fb5c`: palette 0 (the halo mask, TEXEL0) and palette 1 (the core,
/// TEXEL1) for v1.
const TLUT_V1_0: [u8; 16] = [0, 0x58, 0x74, 0x90, 0xac, 0xc8, 0xe4, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
const TLUT_V1_1: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0x18, 0x30, 0x5c, 0x88, 0xb4, 0xd8, 0xff];

/// `SPACE_WIDTH` (`text.c:17`).
const SPACE_WIDTH: i32 = 5;

impl Font {
    /// `text_load_font` (`text.c:184`): big-endian, 13×13 kerning then 94
    /// chars whose `pixeldata` is an offset into the file.
    pub fn load(path: &Path) -> Result<Font, String> {
        let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if data.len() < 676 + 94 * 12 {
            return Err(format!("{}: too short", path.display()));
        }
        let be32 = |o: usize| i32::from_be_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
        let kerning = (0..169).map(|i| be32(i * 4)).collect();
        let chars = (0..94)
            .map(|i| {
                let o = 676 + i * 12;
                FontChar {
                    index: data[o],
                    baseline: data[o + 1] as i8 as i32,
                    height: data[o + 2] as i32,
                    width: data[o + 3] as i32,
                    kerningindex: be32(o + 4),
                    pixeldata: be32(o + 8) as u32 as usize,
                }
            })
            .collect();
        Ok(Font { kerning, chars, data })
    }

    fn ch(&self, c: u8) -> &FontChar {
        &self.chars[(c.clamp(0x21, 0x21 + 93) - 0x21) as usize]
    }

    /// The CI4 index of texel (s, t) of a glyph (out of its rows: 0).
    fn texel(&self, fc: &FontChar, s: i32, t: i32) -> usize {
        if !(0..16).contains(&s) || t < 0 {
            return 0;
        }
        let o = fc.pixeldata + t as usize * 8 + s as usize / 2;
        let Some(&b) = self.data.get(o) else { return 0 };
        (if s % 2 == 0 { b >> 4 } else { b & 15 }) as usize
    }

    fn kern(&self, prev: &FontChar, cur: &FontChar) -> i32 {
        self.kerning.get((prev.kerningindex * 13 + cur.kerningindex) as usize).copied().unwrap_or(0)
    }

    /// `chars['[']` — PD indexes the array directly here, so it is the char
    /// 0x7c's metrics that set the line height.
    fn lineheight(&self) -> i32 {
        let c = &self.chars[b'[' as usize];
        c.height + c.baseline
    }

    /// `text_measure` (`text.c:2256`, NTSC): (height, width).
    pub fn measure(&self, text: &str) -> (i32, i32) {
        let lineheight = self.lineheight();
        let (mut h, mut w, mut longest) = (0, 0, 0);
        let mut prev = b'H';
        let b = text.as_bytes();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b' ' => {
                    if b.get(i + 1) != Some(&b'\n') {
                        w += SPACE_WIDTH;
                    }
                    prev = b'H';
                }
                b'\n' => {
                    longest = longest.max(w);
                    w = 0;
                    h += lineheight;
                }
                c => {
                    let tmp = self.kern(self.ch(prev), self.ch(c)) - 1;
                    w += self.ch(c).width - tmp;
                    prev = c;
                }
            }
            i += 1;
        }
        (h, w.max(longest))
    }

    /// `text_render_v1` (`text.c:2098`) with `shadowoffset` 0: each glyph in
    /// `textcolour` over a `glowcolour` halo (2-cycle: colour = lerp(glow,
    /// text, TEXEL1 α), alpha = TEXEL0 α × text α).
    pub fn render_v1(&self, cv: &mut Canvas, x: i32, y: i32, text: &str, textcolour: u32, glowcolour: u32) {
        let (trgb, ta) = split(textcolour);
        let (grgb, _) = split(glowcolour);
        let lineheight = self.lineheight();
        let (savedx, savedy) = (x, y);
        let (width, height) = (cv.w as i32, cv.h as i32);
        let (mut x, mut y) = (x, y);
        let mut prev = b'H';
        for &c in text.as_bytes() {
            match c {
                b' ' => {
                    x += SPACE_WIDTH;
                    prev = b'H';
                }
                b'\n' => {
                    x = savedx;
                    y += lineheight;
                    prev = b'H';
                }
                c => {
                    let cur = *self.ch(c);
                    // text_render_char_v1 (text.c:1988)
                    x -= self.kern(self.ch(prev), &cur) - 1;
                    let vis = x > 0
                        && x <= cv.w as i32
                        && y + cur.baseline <= cv.h as i32
                        && x <= savedx + width
                        && cur.baseline + y <= savedy + height
                        && x >= savedx
                        && y + cur.baseline + cur.height >= savedy;
                    if vis {
                        // text_render_char_v1_part2: the (w+2)×(h+2) texel
                        // block at (x − 1, y − 1 + baseline), s/t from 0.
                        let (ox, oy) = (x - 1, y - 1 + cur.baseline);
                        for t in 0..cur.height + 2 {
                            for s in 0..cur.width + 2 {
                                let idx = self.texel(&cur, s, t);
                                let a0 = TLUT_V1_0[idx] as f32 / 255.0;
                                let a1 = TLUT_V1_1[idx] as f32 / 255.0;
                                let rgb = [0, 1, 2].map(|k| (trgb[k] - grgb[k]) * a1 + grgb[k]);
                                cv.blend(ox + s, oy + t, rgb, a0 * ta);
                            }
                        }
                    }
                    x += cur.width;
                    prev = c;
                }
            }
        }
    }

    /// `text_render_v2` (`text.c:1741`) + `text_render_char_v2` (`:1453`) for
    /// one line: glyph cores only (`var8007fb3c`), each char drawn only if it
    /// fits inside `width` from `x` (the HUD reveals names this way), and its
    /// colour through `text_get_colour_at_pos` with the HUD's wave blend
    /// (`text_set_wave_blend(frac·50, 0, 50)`, both wave colours white).
    pub fn render_v2(&self, cv: &mut Canvas, x: i32, y: i32, text: &str, colour: u32, width: i32, wave: Option<f32>) {
        let (savedx, savedy) = (x, y);
        let height = 1000;
        let (mut x, mut prev) = (x, b'H');
        for &c in text.as_bytes() {
            match c {
                b' ' => {
                    x += SPACE_WIDTH;
                    prev = b'H';
                }
                b'\n' => break,
                c => {
                    let cur = *self.ch(c);
                    x -= self.kern(self.ch(prev), &cur) - 1;
                    let inside = x > 0
                        && x <= cv.w as i32
                        && y + cur.baseline <= cv.h as i32
                        && savedx + width >= x
                        && savedy + height >= cur.baseline + y
                        && x >= savedx
                        && cur.baseline + y + cur.height >= savedy;
                    if inside && x + cur.width <= savedx + width {
                        let col = match wave {
                            Some(w) => wave_colour(x, y, colour, w),
                            None => colour,
                        };
                        let (rgb, pa) = split(col);
                        // gSPTextureRectangle from (x, y + baseline), s/t from 1.
                        for t in 0..cur.height {
                            for s in 0..cur.width {
                                let a = TLUT_V2[self.texel(&cur, s + 1, t + 1)] as f32 / 255.0;
                                cv.blend(x + s, y + cur.baseline + t, rgb, a * pa);
                            }
                        }
                    }
                    x += cur.width;
                    prev = c;
                }
            }
        }
    }
}

/// `text_get_colour_at_pos`'s `BLENDTYPE_WAVE` branch (`text.c:821`) with
/// `wave4c = frac·50`, `wave50 = 0`, `wave54 = 50`, both colours white:
/// `g_TextSubtleTX` 60 / `g_TextSubleTY` 128.
fn wave_colour(x: i32, y: i32, colour: u32, wave4c: f32) -> u32 {
    let mut f0 = (wave4c as i32 - x + 0 - y + 800) as f32;
    f0 = 4.0 * f0 / 50.0;
    f0 -= ((f0 * 0.25) as i32) as f32 * 4.0;
    f0 -= 1.0;
    if f0 > 1.0 {
        f0 = 2.0 - f0;
    }
    let white = 0xffffff00 | (colour & 0xff);
    if f0 < 0.0 {
        colour_blend(white, colour, (60.0 * -f0) as u32)
    } else {
        colour_blend(white, colour, (128.0 * f0) as u32)
    }
}
