//! GoldenEye radial health/armor HUD image processor (P5) — a CPU port of
//! `3DS FPS/src/ui/HealthHUD.ts`. Loads the health graphic, keys black to
//! transparent, and builds per-pixel angle/side maps once; `render` then bakes an
//! RGBA image for a given health/armor fraction with per-segment depletion (top
//! bars darken first). Left arc = health, right arc = armor. The renderer uploads
//! the baked RGBA to a texture and draws it full-screen.

const DEPLETED_BRIGHTNESS: f32 = 0.15;
const DEPLETED_ALPHA: f32 = 0.2;
/// Sum-of-RGB below this → treat the texel as background (→ transparent).
const BLACK_THRESHOLD: u16 = 30;
/// Width of the smooth depletion transition along the arc (JS `BLUR_WIDTH`).
const BLUR_WIDTH: f32 = 0.08;

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The processed health graphic: the base (black-keyed) RGBA plus the per-pixel
/// normalized arc angle (`0` = bottom, `1` = top; `-1` = transparent) and side
/// (`0` = health/left, `1` = armor/right, `255` = transparent).
pub struct HealthHud {
    pub w: u32,
    pub h: u32,
    base: Vec<u8>,
    angle_map: Vec<f32>,
    side_map: Vec<u8>,
}

impl HealthHud {
    /// Load + process the health JPEG (JS `processImage`). `None` if it fails.
    pub fn load(path: &str) -> Option<Self> {
        let img = image::open(path).ok()?.to_rgba8();
        let (w, h) = img.dimensions();
        let mut base = img.into_raw();
        // Black → transparent.
        for px in base.chunks_exact_mut(4) {
            if (px[0] as u16 + px[1] as u16 + px[2] as u16) < BLACK_THRESHOLD {
                px[3] = 0;
            }
        }

        let n = (w * h) as usize;
        let mut angle_map = vec![-1.0f32; n];
        let mut side_map = vec![255u8; n];
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let angle_at = |x: u32, y: u32| ((x as f32 - cx).abs()).atan2(y as f32 - cy);

        // Pass 1: per-side angle range.
        let (mut hmin, mut hmax) = (f32::INFINITY, f32::NEG_INFINITY);
        let (mut amin, mut amax) = (f32::INFINITY, f32::NEG_INFINITY);
        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) as usize;
                if base[idx * 4 + 3] == 0 {
                    continue;
                }
                let a = angle_at(x, y);
                if (x as f32) < cx {
                    hmin = hmin.min(a);
                    hmax = hmax.max(a);
                } else {
                    amin = amin.min(a);
                    amax = amax.max(a);
                }
            }
        }
        // Pass 2: normalized angle + side.
        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) as usize;
                if base[idx * 4 + 3] == 0 {
                    continue;
                }
                let is_health = (x as f32) < cx;
                let a = angle_at(x, y);
                let (mina, maxa) = if is_health { (hmin, hmax) } else { (amin, amax) };
                let range = maxa - mina;
                if range <= 0.0 {
                    continue;
                }
                angle_map[idx] = (a - mina) / range;
                side_map[idx] = if is_health { 0 } else { 1 };
            }
        }

        Some(HealthHud { w, h, base, angle_map, side_map })
    }

    /// Bake the RGBA for the given health / armor fractions (0..1), depleting from
    /// the top of each arc (JS `renderHealth`).
    pub fn render(&self, health_pct: f32, armor_pct: f32) -> Vec<u8> {
        let mut out = vec![0u8; self.base.len()];
        for i in 0..(self.w * self.h) as usize {
            let alpha = self.base[i * 4 + 3];
            if alpha == 0 {
                continue;
            }
            let t = self.angle_map[i];
            if t < 0.0 {
                continue;
            }
            let pct = if self.side_map[i] == 0 { health_pct } else { armor_pct };
            // fade: 0 = fully lit (below the depletion line), 1 = depleted (above).
            let fade = smoothstep(pct - BLUR_WIDTH, pct + BLUR_WIDTH, t);
            let brightness = 1.0 - fade * (1.0 - DEPLETED_BRIGHTNESS);
            let alpha_scale = 1.0 - fade * (1.0 - DEPLETED_ALPHA);
            out[i * 4] = (self.base[i * 4] as f32 * brightness) as u8;
            out[i * 4 + 1] = (self.base[i * 4 + 1] as f32 * brightness) as u8;
            out[i * 4 + 2] = (self.base[i * 4 + 2] as f32 * brightness) as u8;
            out[i * 4 + 3] = (alpha as f32 * alpha_scale) as u8;
        }
        out
    }
}
