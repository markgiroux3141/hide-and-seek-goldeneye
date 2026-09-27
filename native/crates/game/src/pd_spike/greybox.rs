//! The greybox: a [`LevelGeom`] drawn as flat-shaded coloured polygons with dark
//! tile outlines. Viewer-only, and built from the generic geometry, so it draws an
//! editor level exactly as it draws a PD stage.
//!
//! The engine has no lit coloured pipeline, so the lighting is baked into vertex
//! colours: a fixed sun, two-sided (`|n·L|`), because source winding is unreliable.
//! Output is in **metres** for the renderer's unlit, depth-tested colour pass.

use glam::Vec3;

use engine::render::mesh::{ColorVertex, ColoredMesh};

use super::arena::UNITS_PER_M;
use super::debug_draw::Rgb;
use super::level_geom::{FloorKind, GeomPoly, LevelGeom};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GreyboxOpts {
    /// Hide polygons that start above this height (cm), to see into lower floors.
    pub clip_y: Option<f32>,
    /// Mix each polygon's colour with a per-room hue.
    pub room_tint: bool,
}

/// Height bands for flat floors (cm): Complex has floors at −280, 0, 280 and 510–550.
pub fn floor_band_colour(y: f32) -> Rgb {
    if y < -100.0 {
        [0.50, 0.38, 0.62] // pit
    } else if y < 140.0 {
        [0.66, 0.61, 0.50] // ground floor
    } else if y < 400.0 {
        [0.38, 0.62, 0.56] // first floor
    } else {
        [0.44, 0.54, 0.80] // top floor
    }
}

pub const RAMP: Rgb = [0.90, 0.56, 0.22];
pub const WALL: Rgb = [0.56, 0.56, 0.60];
/// A wall that blocks neither sight nor shots (railings, windows).
pub const SEE_THROUGH: Rgb = [0.78, 0.90, 0.96];
const RISER: Rgb = [0.66, 0.52, 0.42];

/// An evenly spread hue for index `k` (golden-ratio steps), as RGB.
pub fn hue(k: usize) -> Rgb {
    hsv((k as f32 * 0.618_034).fract() * 6.0)
}

/// Like [`hue`] but confined to green → blue → violet (90°–280°), for overlays that
/// sit beside the reserved signal colours: yellow link, orange one-way link, red
/// error, magenta cover.
pub fn cool_hue(k: usize) -> Rgb {
    hsv((90.0 + (k as f32 * 0.618_034).fract() * 190.0) / 60.0)
}

/// Full-saturation hue `h` in sextants (0..6), lifted towards white a little.
fn hsv(h: f32) -> Rgb {
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let (r, g, b) = match h as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    [0.25 + 0.7 * r, 0.25 + 0.7 * g, 0.25 + 0.7 * b]
}

pub fn base_colour(p: &GeomPoly) -> Rgb {
    match p.floor_kind() {
        Some(FloorKind::Flat) => floor_band_colour(p.min_y()),
        Some(FloorKind::Ramp) => RAMP,
        Some(FloorKind::Vertical) => RISER,
        None if !p.blocks_sight => SEE_THROUGH,
        None => WALL,
    }
}

fn shade(c: Rgb, k: f32) -> Rgb {
    [c[0] * k, c[1] * k, c[2] * k]
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

pub fn is_visible(p: &GeomPoly, opts: &GreyboxOpts) -> bool {
    opts.clip_y.map_or(true, |c| p.min_y() <= c)
}

pub fn build(geom: &LevelGeom, opts: &GreyboxOpts) -> ColoredMesh {
    let sun = Vec3::new(0.35, 0.85, 0.4).normalize();
    // Outline band width and its lift off the face (metres), both sides of it.
    const EDGE: f32 = 0.03;
    const LIFT: f32 = 0.006;
    let mut mesh = ColoredMesh::default();
    for p in geom.polys.iter().filter(|p| is_visible(p, opts)) {
        let mut col = base_colour(p);
        if opts.room_tint {
            if let Some(r) = p.room {
                col = mix(col, hue(r as usize), 0.45);
            }
        }
        let lit = shade(col, 0.55 + 0.45 * p.normal.dot(sun).abs());
        let verts: Vec<Vec3> = p.verts.iter().map(|v| *v / UNITS_PER_M).collect();
        let base = mesh.vertices.len() as u32;
        for v in &verts {
            mesh.vertices.push(ColorVertex { pos: (*v).into(), color: lit });
        }
        for i in 1..verts.len() as u32 - 1 {
            mesh.indices.extend_from_slice(&[base, base + i, base + i + 1]);
        }

        // Outlines: an inset band along each edge, lifted off both faces so it wins
        // the depth test from either side (the colour pass does not cull).
        let n = p.normal;
        if n == Vec3::ZERO {
            continue;
        }
        let centre = verts.iter().copied().sum::<Vec3>() / verts.len() as f32;
        let dark = shade(lit, 0.45);
        for side in [1.0f32, -1.0] {
            let lift = n * (LIFT * side);
            for i in 0..verts.len() {
                let (a, b) = (verts[i], verts[(i + 1) % verts.len()]);
                let mut inward = n.cross(b - a).normalize_or_zero();
                if inward.dot(centre - a) < 0.0 {
                    inward = -inward;
                }
                let w = inward * EDGE;
                let q = mesh.vertices.len() as u32;
                for v in [a + lift, b + lift, b + w + lift, a + w + lift] {
                    mesh.vertices.push(ColorVertex { pos: v.into(), color: dark });
                }
                mesh.indices.extend_from_slice(&[q, q + 1, q + 2, q, q + 2, q + 3]);
            }
        }
    }
    mesh
}
