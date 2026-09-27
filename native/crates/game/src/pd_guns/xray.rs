//! The Farsight XR-20's x-ray (`VISIONMODE_XRAY`): the eraser sphere set up in
//! `bg_tick` (`bg.c:5253`), the BG drawn only inside it with PD's per-vertex
//! colours (`bg_render_scene_in_xray` / `bg_render_gdl_in_xray` /
//! `bg_choose_xray_vtx_colour`, `bg.c:891`, `:760`, `:451`), and the prop
//! colouring rule every x-ray prop path shares (objects `propobj.c:12720` /
//! `:12842`, smoke `smoke.c:180`, explosions `explosions.c:1314`, sparks
//! `sparks.c:333`).
//!
//! Substituted: PD's rooms are tessellated finely enough for per-vertex colour
//! to read as a sphere (the Complex/CI stages have `unk2c = -1`, so
//! `bg_process_xray_tri` never subdivides). The range's walls are single
//! 36 m quads, so [`bg_geometry`] cuts every face into [`XRAY_TESS`] cm cells
//! first, standing in for a PD room's own vertex density.

use glam::Vec3;

use super::fx::{FxBatch, FxKind, FxVert};
use super::range::{Aabb, Range};

/// Cell size (cm) the range's faces are cut into for the x-ray.
pub const XRAY_TESS: f32 = 50.0;

/// `g_Stages[STAGEINDEX_CITRAINING]`: `eraserpropdist` 400, `unk30` 0.
pub const STAGE_ERASERPROPDIST: f32 = 400.0;
pub const STAGE_UNK30: f32 = 0.0;

/// The player's eraser state (`player->eraserpos`, `eraserpropdist`,
/// `eraserbgdist`, `ecol_1..3`, `epcol_0..2`).
#[derive(Clone, Copy, Debug)]
pub struct Eraser {
    pub pos: Vec3,
    pub propdist: f32,
    pub bgdist: f32,
    /// Bit shifts into an RGBA8888 word: 24 red, 16 green, 8 blue.
    pub ecol: [u32; 3],
    /// Channel indexes into an object's colour.
    pub epcol: [usize; 3],
}

impl Eraser {
    /// The Farsight's colours (`bondgun.c:8015`): BG near green → red, far
    /// blue + red; props red → green.
    pub fn farsight(pos: Vec3) -> Self {
        Eraser {
            pos,
            propdist: STAGE_ERASERPROPDIST,
            bgdist: STAGE_ERASERPROPDIST + STAGE_UNK30,
            ecol: [16, 24, 8],
            epcol: [0, 1, 2],
        }
    }

    /// The fade every x-ray prop shares: `None` beyond `eraserpropdist`, else
    /// (distance frac 0..1, alpha frac 1 → 0 over the last 150 cm).
    pub fn prop_fade(&self, pos: Vec3) -> Option<(f32, f32)> {
        let dist = (pos - self.pos).length();
        if dist > self.propdist {
            return None;
        }
        let fadedist = self.propdist - 150.0;
        let alpha = if dist > fadedist { 1.0 - (dist - fadedist) / 150.0 } else { 1.0 };
        Some(((dist / self.propdist).min(1.0), alpha))
    }

    /// An object's x-ray colour (`propobj.c:12720`, `:12842`): alpha 128
    /// fading out, `colour[epcol_0] = frac·255`, `colour[epcol_1] =
    /// (1 − frac)·255`, `colour[epcol_2] = 0`, and a fog weight of 0xff, so the
    /// model is drawn flat in that colour. Returns (rgb 0..1, alpha 0..1).
    pub fn obj_colour(&self, pos: Vec3) -> Option<[f32; 4]> {
        let (frac, fade) = self.prop_fade(pos)?;
        let alpha = (fade * 128.0).floor();
        if alpha <= 0.0 {
            return None;
        }
        let mut c = [0.0f32; 4];
        c[self.epcol[0]] = (frac * 255.0).floor() / 255.0;
        c[self.epcol[1]] = ((1.0 - frac) * 255.0).floor() / 255.0;
        c[self.epcol[2]] = 0.0;
        c[3] = alpha / 255.0;
        Some(c)
    }

    /// Smoke in x-ray (`smoke.c:180`): red → green with distance, the part's
    /// alpha × 0.5 fading out.
    pub fn smoke_colour(&self, pos: Vec3, alpha: f32) -> Option<[f32; 4]> {
        let (frac, fade) = self.prop_fade(pos)?;
        let a = ((alpha * fade * 0.5) as u32 & 0xff) as f32;
        Some([(frac * 255.0).floor() / 255.0, ((1.0 - frac) * 255.0).floor() / 255.0, 0.0, a / 255.0])
    }

    /// An explosion in x-ray (`explosions.c:1314`): `red << 24 | green << 16 |
    /// alpha | 0x80800000`, red/green 0..127 over half-bright bases.
    pub fn explosion_colour(&self, pos: Vec3) -> Option<[f32; 4]> {
        let (frac, fade) = self.prop_fade(pos)?;
        let alpha = (fade * 128.0) as u32;
        let red = (frac * 127.0) as u32;
        let green = ((1.0 - frac) * 127.0) as u32;
        Some([(0x80 | red) as f32 / 255.0, (0x80 | green) as f32 / 255.0, 0.0, alpha as f32 / 255.0])
    }

    /// A spark group in x-ray (`sparks.c:375`): red/green by distance, blue
    /// 0x3f, alpha from `unk1c` scaled by the fade (PD's @bug: both colours
    /// read `unk1c`).
    pub fn spark_colour(&self, pos: Vec3, unk1c_alpha: f32) -> Option<[f32; 4]> {
        let dist = (pos - self.pos).length();
        if dist > self.propdist {
            return None;
        }
        let f12 = self.propdist - 150.0;
        let sp138 = if f12 < dist { 1.0 - (dist - f12) / 150.0 } else { 1.0 };
        let frac = (dist / self.propdist).min(1.0);
        let a = (sp138 * unk1c_alpha * 255.0) as u32 as f32;
        Some([(frac * 255.0).floor() / 255.0, ((1.0 - frac) * 255.0).floor() / 255.0, 0x3f as f32 / 255.0, a / 255.0])
    }
}

/// `struct xraydata` as `bg_render_gdl_in_xray` fills it (`bg.c:771`).
struct XrayData {
    /// unk000..008: the eraser, room-relative (the range is one room at 0).
    centre: Vec3,
    /// unk00c / unk010.
    radius: f32,
    radius_sq: f32,
    /// unk014: where the alpha starts fading.
    fadefrom: f32,
    /// unk01c: where the near colour band ends.
    near: f32,
}

impl XrayData {
    fn new(e: &Eraser) -> Self {
        let radius = e.bgdist;
        XrayData { centre: e.pos, radius, radius_sq: radius * radius, fadefrom: 0.25, near: (e.propdist / radius).min(0.7) }
    }
}

/// `bg_choose_xray_vtx_colour` (`bg.c:451`): `None` out of range (PD's
/// `0x0000ff00`, invisible), else the vertex's RGBA word decoded.
fn bg_choose_xray_vtx_colour(v: Vec3, xd: &XrayData, ecol: [u32; 3]) -> Option<[f32; 4]> {
    let d = v - xd.centre;
    let (dx, dy, dz) = (d.x * d.x, d.y * d.y, d.z * d.z);
    if dx >= xd.radius_sq || dz >= xd.radius_sq || dy >= xd.radius_sq {
        return None;
    }
    let dist = (dx + dy + dz).sqrt();
    if dist >= xd.radius {
        return None;
    }
    let f12 = dist / xd.radius;
    let alphafrac = if xd.fadefrom < f12 { 1.0 - (f12 - xd.fadefrom) / (1.0 - xd.fadefrom) } else { 1.0 };
    let word: u32 = if f12 < xd.near {
        let anglefrac = f12 / xd.near;
        let colfrac = ((1.0 - anglefrac) * std::f32::consts::FRAC_PI_2).sin();
        ((colfrac * 255.0) as u32) << ecol[0] | (((1.0 - colfrac) * 255.0) as u32) << ecol[1] | (alphafrac * 128.0) as u32
    } else {
        let anglefrac = (f12 - xd.near) / (1.0 - xd.near);
        let anglefrac = 0.65 * anglefrac + 0.35;
        let colfrac = (anglefrac * std::f32::consts::FRAC_PI_2).sin();
        ((colfrac * 255.0) as u32) << ecol[2] | 0xff << ecol[1] | (alphafrac * 128.0) as u32
    };
    Some(unpack(word))
}

fn unpack(w: u32) -> [f32; 4] {
    [(w >> 24 & 0xff) as f32 / 255.0, (w >> 16 & 0xff) as f32 / 255.0, (w >> 8 & 0xff) as f32 / 255.0, (w & 0xff) as f32 / 255.0]
}

/// The BG in x-ray: every range face, tessellated, each triangle drawn when
/// any vertex is in range (`bg_process_xray_tri` with no subdivision →
/// `bg_add_xray_tri`), out-of-range vertices transparent. Drawn with
/// `G_CC_SHADE` / `G_RM_AA_XLU_SURF`: no z, no cull ([`FxKind::XrayBg`]).
pub fn bg_geometry(range: &Range, e: &Eraser) -> FxBatch {
    let xd = XrayData::new(e);
    let mut verts = Vec::new();
    let reach = Aabb::new(e.pos - Vec3::splat(xd.radius), e.pos + Vec3::splat(xd.radius));
    // A PD stage: its own BG triangles, each drawn when any vertex is in range
    // (no subdivision, as for the range's faces).
    if let Some(tris) = &range.xray_tris {
        for tri in tris.iter() {
            let (mn, mx) = (tri[0].min(tri[1]).min(tri[2]), tri[0].max(tri[1]).max(tri[2]));
            if mx.cmplt(reach.min).any() || mn.cmpgt(reach.max).any() {
                continue;
            }
            let cols = tri.map(|p| bg_choose_xray_vtx_colour(p, &xd, e.ecol));
            if cols.iter().all(|c| c.is_none()) {
                continue;
            }
            for (p, c) in tri.iter().zip(cols) {
                verts.push(FxVert { pos: *p, st: [0.0, 0.0], col: c.unwrap_or([0.0, 0.0, 1.0, 0.0]) });
            }
        }
    }
    for (origin, u, v) in faces(range) {
        // Skip faces the eraser's box can't touch.
        let (a, b) = (origin, origin + u + v);
        let (mn, mx) = (a.min(b), a.max(b));
        if mx.cmplt(reach.min).any() || mn.cmpgt(reach.max).any() {
            continue;
        }
        let nu = (u.length() / XRAY_TESS).ceil().max(1.0) as usize;
        let nv = (v.length() / XRAY_TESS).ceil().max(1.0) as usize;
        let at = |i: usize, j: usize| origin + u * (i as f32 / nu as f32) + v * (j as f32 / nv as f32);
        let mut cols = vec![None; (nu + 1) * (nv + 1)];
        for j in 0..=nv {
            for i in 0..=nu {
                cols[j * (nu + 1) + i] = bg_choose_xray_vtx_colour(at(i, j), &xd, e.ecol);
            }
        }
        let col = |i: usize, j: usize| cols[j * (nu + 1) + i];
        for j in 0..nv {
            for i in 0..nu {
                for tri in [[(i, j), (i + 1, j), (i + 1, j + 1)], [(i, j), (i + 1, j + 1), (i, j + 1)]] {
                    if tri.iter().all(|&(a, b)| col(a, b).is_none()) {
                        continue;
                    }
                    for (a, b) in tri {
                        // 0x0000ff00: blue at alpha 0.
                        let c = col(a, b).unwrap_or([0.0, 0.0, 1.0, 0.0]);
                        verts.push(FxVert { pos: at(a, b), st: [0.0, 0.0], col: c });
                    }
                }
            }
        }
    }
    FxBatch { kind: FxKind::XrayBg, verts }
}

/// The range's BG faces as (corner, edge u, edge v): the room shell's six
/// inner faces, then each box's sides and top (their bottoms sit on the floor).
fn faces(range: &Range) -> Vec<(Vec3, Vec3, Vec3)> {
    let mut out = Vec::new();
    let mut add_box = |b: &Aabb, bottom: bool| {
        let (mn, mx) = (b.min, b.max);
        let d = mx - mn;
        let (x, y, z) = (Vec3::new(d.x, 0.0, 0.0), Vec3::new(0.0, d.y, 0.0), Vec3::new(0.0, 0.0, d.z));
        out.push((mn, x, y));
        out.push((mn + z, x, y));
        out.push((mn, z, y));
        out.push((mn + x, z, y));
        out.push((mn + y, x, z));
        if bottom {
            out.push((mn, x, z));
        }
    };
    add_box(&range.bounds, true);
    for s in &range.solids {
        add_box(s, false);
    }
    out
}

/// A target board in x-ray: its box's camera-facing faces in the object's
/// colour (the board is a PD object; `G_RM_AA_ZB_XLU_SURF` never writes z, so
/// only its front faces would show through each other anyway).
pub fn board_geometry(bbox: &Aabb, campos: Vec3, col: [f32; 4], out: &mut Vec<FxVert>) {
    let (mn, mx) = (bbox.min, bbox.max);
    let c = (mn + mx) * 0.5;
    for axis in 0..3 {
        for side in [-1.0f32, 1.0] {
            let mut n = Vec3::ZERO;
            n[axis] = side;
            let mut centre = c;
            centre[axis] = if side < 0.0 { mn[axis] } else { mx[axis] };
            if n.dot(campos - centre) <= 0.0 {
                continue;
            }
            let (a1, a2) = ((axis + 1) % 3, (axis + 2) % 3);
            let corner = |s1: f32, s2: f32| {
                let mut p = centre;
                p[a1] = if s1 < 0.0 { mn[a1] } else { mx[a1] };
                p[a2] = if s2 < 0.0 { mn[a2] } else { mx[a2] };
                FxVert { pos: p, st: [0.0, 0.0], col }
            };
            let q = [corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)];
            out.extend_from_slice(&[q[0], q[1], q[2], q[0], q[2], q[3]]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn farsight_vertex_colours_run_green_to_red_to_blue_and_fade() {
        let e = Eraser::farsight(Vec3::ZERO);
        let xd = XrayData::new(&e);
        // At the centre: full green (ecol_1 = 16), alpha 128.
        let c = bg_choose_xray_vtx_colour(Vec3::ZERO, &xd, e.ecol).unwrap();
        assert_eq!(c, [0.0, 1.0, 0.0, 128.0 / 255.0]);
        // Past the near band (0.7 · 400 cm): red 0xff plus some blue, fading.
        let c = bg_choose_xray_vtx_colour(Vec3::new(300.0, 0.0, 0.0), &xd, e.ecol).unwrap();
        assert_eq!(c[0], 1.0);
        assert!(c[2] > 0.5 && c[3] < 0.5, "{c:?}");
        assert!(bg_choose_xray_vtx_colour(Vec3::new(401.0, 0.0, 0.0), &xd, e.ecol).is_none());
    }
}
