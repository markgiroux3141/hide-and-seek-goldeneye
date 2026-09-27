//! `game/nbomb.c` — the N-Bomb storm, ported (NTSC final).
//!
//! A black, textured geodesic dome (an octahedron subdivided twice) that grows
//! to 5 m in 80 ticks, breathes, spins, darkens the room and fades after 310
//! ticks; standing inside it paints the storm texture over the screen
//! (`nbomb_render_overlay`). Texture: `g_TcGeneralConfigs[TEX_GENERAL_NBOMBDOME]`
//! (TEXTURE_063B, IA8 64×64, wrap), `G_CC_MODULATEIA`, `G_RM_ZB_XLU_SURF`, no cull.

use glam::{Mat4, Vec3};

use super::bgun::Lv;
use super::fx::{FxBatch, FxKind, FxVert};
use super::pdmtx;
use super::props::pd_atan2f;
use crate::pd_spike::pdmath::dtor;

pub const TEX_NBOMBDOME: u16 = 0x063b;

/// `struct nbomb`.
#[derive(Clone, Copy, Debug)]
pub struct Nbomb {
    pub age240: i32,
    pub pos: Vec3,
    pub radius: f32,
    pub unk14: i32,
    pub unk18: f32,
}

impl Default for Nbomb {
    fn default() -> Self {
        Nbomb { age240: -1, pos: Vec3::ZERO, radius: 0.0, unk14: 0, unk18: 0.0 }
    }
}

/// `g_Nbombs[6]`.
#[derive(Default)]
pub struct Nbombs {
    pub bombs: [Nbomb; 6],
    /// The hum is playing (`g_NbombAudioHandle`).
    pub humming: bool,
}

/// What a storm asks of the world this tick.
#[derive(Default, Debug)]
pub struct NbombOut {
    /// `room_flash_lighting(room, -38, -180)`: the storm darkens the room.
    pub darken: bool,
    /// The player is inside a storm: chr_damage_by_dizziness (recorded only).
    pub dizzy: f32,
    /// Start / stop the storm hum (SFXMAP_810C_SHIP_HUM).
    pub hum_start: bool,
    pub hum_stop: bool,
    /// The two SFXNUM_0001 roars at 0.4 pitch (`nbomb_create_storm`).
    pub roars: u32,
}

/// `nbomb_calculate_alpha` (`nbomb.c:291`).
pub fn nbomb_calculate_alpha(n: &Nbomb) -> i32 {
    if n.age240 > 310 {
        if n.age240 < 350 {
            (350 * 127 - n.age240 * 127) / 40
        } else {
            0
        }
    } else {
        127
    }
}

impl Nbombs {
    /// `nbomb_create_storm` (`nbomb.c:663`).
    pub fn create_storm(&mut self, pos: Vec3, out: &mut NbombOut) {
        let mut oldest = -1;
        let mut index = 0;
        for (i, b) in self.bombs.iter().enumerate() {
            if b.age240 == -1 {
                index = i;
                break;
            }
            if b.age240 > oldest {
                index = i;
                oldest = b.age240;
            }
        }
        self.bombs[index] = Nbomb { age240: 0, pos, radius: 0.0, unk14: 0, unk18: 0.0 };
        out.roars += 2;
    }

    pub fn active(&self) -> bool {
        self.bombs.iter().any(|b| b.age240 >= 0)
    }

    /// `nbombs_tick` + `nbomb_tick` + `nbomb_inflict_damage` (`nbomb.c:522`).
    pub fn tick(&mut self, lv: Lv, playerpos: Vec3, out: &mut NbombOut) {
        if lv.lvupdate240 == 0 {
            return;
        }
        let mut youngest = 20000;
        for n in self.bombs.iter_mut() {
            if n.age240 < 0 {
                continue;
            }
            let increment = (lv.lvupdate240 + 2) >> 2;
            n.age240 += increment;
            if n.age240 < 80 {
                n.radius = (n.age240 as f32 / 80.0).sqrt().sqrt();
                n.unk18 = 0.0;
            } else {
                n.radius = ((n.age240 - 80) as f32 * 0.052_333_336).sin() * 0.05 + 1.0;
                n.unk18 = (n.age240 - 80) as f32 / 270.0 * 3.0;
            }
            n.radius *= 500.0;
            // nbomb_inflict_damage: darken the room; dizzy whoever's inside.
            if n.age240 <= 350 {
                out.darken = true;
                if (playerpos - n.pos).length() < n.radius {
                    out.dizzy += 0.01 * lv.lvupdate60freal;
                }
            }
            let age60 = (n.age240 / 4).min(40);
            n.unk14 = (n.unk14 + increment * age60) % 0x800;
            if n.age240 < youngest {
                youngest = n.age240;
            }
            if n.age240 > 370 {
                n.age240 = -1;
            }
        }
        if youngest < 350 {
            if !self.humming {
                self.humming = true;
                out.hum_start = true;
            }
        } else if self.humming {
            self.humming = false;
            out.hum_stop = true;
        }
    }

    /// `nbombs_render` → `nbomb_create_gdl` + `nbomb_render` (`nbomb.c:311`,
    /// `:364`). `interval_frac` is `g_20SecIntervalFrac`. (PD builds the display
    /// list before `nbomb_render` sets the 2000 unit scale, so its very first
    /// frame draws a tiny dome; not reproduced.)
    pub fn geometry(&self, interval_frac: f32) -> Vec<(Vec3, FxBatch)> {
        let mut out = Vec::new();
        let cb00 = ((interval_frac * 64.0 * 32.0 * 16.0) as i32 % 0x800) as f32;
        for n in self.bombs.iter().filter(|n| n.age240 >= 0) {
            let alpha = nbomb_calculate_alpha(n) as f32 / 255.0;
            let mut mtx = pdmtx::load_rotation(Vec3::new(0.0, n.unk14 as f32 / 2048.0 * dtor(360.0), 0.0));
            pdmtx::scale3(&mut mtx, n.radius / 2000.0);
            let mtx = Mat4::from_translation(n.pos) * mtx;
            let mut verts = Vec::new();
            geodesic_dome(2, 2000.0, cb00, &mut |v: Vec3, st: [f32; 2]| {
                verts.push(FxVert { pos: mtx.transform_point3(v), st, col: [0.0, 0.0, 0.0, alpha] });
            });
            out.push((n.pos, FxBatch { kind: FxKind::Nbomb, verts }));
        }
        out
    }

    /// `nbomb_render_overlay` (`nbomb.c:784`): inside a storm, the texture over
    /// the whole view. Returns the overlay alpha and the texture scroll (s, t
    /// in texels) — the renderer lays the quad over the screen.
    pub fn overlay(&self, campos: Vec3, interval_frac: f32) -> Option<(f32, [f32; 2])> {
        let mut finalalpha = 0;
        let mut inside = false;
        for n in self.bombs.iter() {
            if n.age240 >= 0 && n.age240 <= 350 && (campos - n.pos).length() < n.radius {
                inside = true;
                finalalpha = finalalpha.max(nbomb_calculate_alpha(n));
            }
        }
        if !inside {
            return None;
        }
        let s = (8.0 * interval_frac * 128.0 * 32.0) as i32 % 2048;
        let t = ((campos.y * 8.0) as i32 % 2048) as i16 as i32 + (2.0 * interval_frac * 128.0 * 32.0) as i16 as i32;
        Some((finalalpha as f32 / 255.0, [s as f32 / 32.0, t as f32 / 32.0]))
    }
}

/// `func0f008558` + `func0f006c80`: the octahedron's eight faces, each split
/// `depth` times (each split makes four triangles from the edge midpoints,
/// pushed out to the sphere). `MAKEVERTEX` gives each vertex s = y·256 and
/// t = around-angle·256 texels + the scroll; the second half's seam vertex
/// (t == 0) is moved to t = 256 (`var8009cb04`).
fn geodesic_dome(depth: i32, scale: f32, cb00: f32, emit: &mut dyn FnMut(Vec3, [f32; 2])) {
    let c = [
        Vec3::new(0.0, 0.0, 1.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(0.0, 0.0, -1.0),
        Vec3::new(-1.0, 0.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(0.0, -1.0, 0.0),
    ];
    let faces_a = [(0, 4, 1), (1, 4, 2), (1, 5, 0), (2, 5, 1)];
    let faces_b = [(2, 4, 3), (3, 4, 0), (3, 5, 2), (0, 5, 3)];
    for (half, faces) in [(false, faces_a), (true, faces_b)] {
        let vert = |v: Vec3| -> (Vec3, [f32; 2]) {
            let s = (v.y * 256.0 * 32.0) as i16 as f32;
            let mut t = (pd_atan2f(v.x, v.z) / dtor(360.0) * 256.0 * 32.0) as i16 as i32;
            if half && t == 0 {
                t = 256 * 32;
            }
            let t = (t + cb00 as i32) as i16 as f32;
            (v * scale, [s / 32.0, t / 32.0])
        };
        for (a, b, cc) in faces {
            split(c[a], c[b], c[cc], depth, &vert, emit);
        }
    }
}

fn split(a: Vec3, b: Vec3, c: Vec3, depth: i32, vert: &dyn Fn(Vec3) -> (Vec3, [f32; 2]), emit: &mut dyn FnMut(Vec3, [f32; 2])) {
    let ab = (a + b).normalize();
    let bc = (b + c).normalize();
    let ca = (c + a).normalize();
    if depth == 0 {
        // gSPTri4(a, ab, ca,  b, bc, ab,  c, ca, bc,  ab, bc, ca)
        for (p, q, r) in [(a, ab, ca), (b, bc, ab), (c, ca, bc), (ab, bc, ca)] {
            for v in [p, q, r] {
                let (pos, st) = vert(v);
                emit(pos, st);
            }
        }
    } else {
        split(a, ab, ca, depth - 1, vert, emit);
        split(b, bc, ab, depth - 1, vert, emit);
        split(c, ca, bc, depth - 1, vert, emit);
        split(ab, bc, ca, depth - 1, vert, emit);
    }
}
