//! The world-side effects the gun code spawns: bullet tracers (`gunfx.c` beams),
//! impact sparks (`sparks.c` / `sparkstick.c`), bullet holes (`wallhit.c`) and
//! ejected casings (`gunfx.c` / `casingtick.c`). Headless: each effect ticks in
//! PD units and emits textured triangles for the renderer ([`FxTri`]).

use glam::{Mat3, Mat4, Vec3};

use super::bgun::Lv;
use super::gset::*;
use super::pdmtx;
use crate::pd_spike::pdmath::{baddtor, Rng};

/// A textured, vertex-coloured triangle in world centimetres.
#[derive(Clone, Copy, Debug)]
pub struct FxVert {
    pub pos: Vec3,
    /// Texels (PD's `s,t / 32`), divided by the texture size in the renderer.
    pub st: [f32; 2],
    pub col: [f32; 4],
}

/// Which texture + combiner a batch of effect triangles uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FxKind {
    /// `G_CC_BLENDIA` (beams): colour = lerp(shade, env, texel), alpha = texel·shade.
    Beam(u16),
    /// `G_CC_CUSTOM_04` (sparks): colour = shade, alpha = texel·shade.
    Spark,
    /// Wallhits: IA texel modulated by the vertex colour, decal z-mode.
    Wallhit(u16),
    /// Untextured vertex colour (the target boards).
    Flat,
    /// `g_TcGdl1` smoke: IA texel 0x002a × shade (`G_CC_MODULATEIA`), wrap.
    Smoke,
    /// `g_TcGdl2` explosion flare frame `i`: flame × colour map
    /// (`G_CC_INTERFERENCE`) × shade (`G_CC_MODULATEIA2`), clamp.
    Explosion(u8),
    /// The N-Bomb dome and screen overlay: IA texel 0x063b × shade, wrap, no cull.
    Nbomb,
    /// A CHRGUNFIRE flash (`model_render_node_chr_gunfire`): a model texture ×
    /// shade (`G_CC_MODULATEIA`, `G_RM_ZB_CLD_SURF`), clamped.
    GunFire(u16),
    /// The BG in x-ray (`bg_render_scene_in_xray`): `G_CC_SHADE`,
    /// `G_RM_AA_XLU_SURF` — no z, no cull, drawn before the props.
    XrayBg,
    /// Props in x-ray (boards): shade only, translucent, no z.
    Xray,
}

pub struct FxBatch {
    pub kind: FxKind,
    pub verts: Vec<FxVert>,
}

// ─── beams (gunfx.c) ──────────────────────────────────────────────────────────

/// `struct beam`.
#[derive(Clone, Debug)]
pub struct Beam {
    pub age: i32,
    pub weaponnum: i32,
    pub from: Vec3,
    pub dir: Vec3,
    pub maxdist: f32,
    pub speed: f32,
    pub mindist: f32,
    pub dist: f32,
}

impl Default for Beam {
    fn default() -> Self {
        Beam { age: -1, weaponnum: 0, from: Vec3::ZERO, dir: Vec3::Z, maxdist: 0.0, speed: 0.0, mindist: 0.0, dist: 0.0 }
    }
}

pub const TEX_BEAM: [u16; 5] = [0x0006, 0x0007, 0x0008, 0x0859, 0x085a];
pub const TEX_BEAM_ORANGE: usize = 0;
pub const TEX_BEAM_BLUE: usize = 1;
pub const TEX_BEAM_YELLOW: usize = 3;
pub const TEX_BEAM_GREEN: usize = 4;
pub const TEX_LASER: u16 = 0x0009;

impl Beam {
    /// `beam_create` (`gunfx.c:27`).
    pub fn create(&mut self, rng: &mut Rng, weaponnum: i32, from: Vec3, to: Vec3) {
        self.from = from;
        let d = to - from;
        let mut distance = d.length();
        self.dir = if distance > 0.0 { d / distance } else { d };
        if distance > 10000.0 {
            distance = 10000.0;
        }
        self.age = 0;
        self.weaponnum = weaponnum;
        self.maxdist = distance;
        if distance < 500.0 {
            distance = 500.0;
        }
        if weaponnum == -1 || weaponnum == -2 {
            self.speed = 0.0;
            self.mindist = distance.min(3000.0);
            self.dist = 0.0;
        } else if weaponnum == WEAPON_LASER {
            self.speed = 0.25 * distance;
            self.mindist = (0.6 * distance).min(3000.0);
            self.dist = (-0.1 - rng.randomfrac() * 0.3) * distance;
        } else {
            self.speed = 0.2 * distance;
            self.mindist = (0.2 * distance).min(3000.0);
            let tmp = rng.randomfrac();
            self.dist = (tmp + tmp - 1.0) * self.speed;
        }
        if self.dist >= self.maxdist {
            self.age = -1;
        }
    }

    /// `beam_tick` (`gunfx.c:600`).
    pub fn tick(&mut self, rng: &mut Rng, lv: Lv) {
        if self.age < 0 {
            return;
        }
        if self.weaponnum == -2 {
            self.age += 1;
            if self.age > 1 {
                self.age = -1;
            }
        } else {
            if lv.lvupdate240 <= 8 {
                self.dist += self.speed * lv.lvupdate60freal;
            } else {
                self.dist += self.speed * (2.0 + rng.randomfrac() * 0.5);
            }
            if self.dist >= self.maxdist {
                self.age = -1;
            }
        }
    }

    /// `beam_render` (`gunfx.c:298`) for the non-laser beams: a quad from the
    /// beam's head back `mindist`, `sp130` wide, facing the camera.
    pub fn geometry(&self, campos: Vec3) -> Option<FxBatch> {
        if self.age < 0 {
            return None;
        }
        let texidx = match self.weaponnum {
            WEAPON_CYCLONE => TEX_BEAM_BLUE,
            WEAPON_TRANQUILIZER => TEX_BEAM_YELLOW,
            WEAPON_MAULER | WEAPON_PHOENIX | WEAPON_CALLISTO | WEAPON_REAPER | WEAPON_FARSIGHT => TEX_BEAM_GREEN,
            w if w <= -3 => TEX_BEAM_GREEN,
            _ => TEX_BEAM_ORANGE,
        };
        let alpha = if self.weaponnum == -1 || self.weaponnum == WEAPON_CYCLONE { 127.0 / 255.0 } else { 1.0 };
        let mut halfwidth = match self.weaponnum {
            WEAPON_LASER => 50.0,
            -2 => 10.0,
            _ => 30.0,
        };
        if self.weaponnum <= -3 {
            halfwidth *= (self.weaponnum + 3) as f32 * 2.0 + 1.0;
        }
        let tex = if self.weaponnum == WEAPON_LASER || self.weaponnum == -2 { TEX_LASER } else { TEX_BEAM[texidx] };
        let mut len = self.mindist;
        let mut dist = self.dist;
        let mut head = self.from;
        if dist > 0.0 {
            head += self.dir * dist;
        } else {
            len += dist;
            dist = 0.0;
        }
        if dist + len > self.maxdist {
            len = self.maxdist - dist;
        }
        if len <= 0.0 {
            return None;
        }
        let tail = head + self.dir * len;
        let mut side = self.dir.cross(campos - tail);
        side = if side != Vec3::ZERO { side.normalize() * halfwidth } else { Vec3::new(0.0, halfwidth, 0.0) };
        // The quad's vertices are in 0.1-unit model space (mtx00015f04(0.1)).
        let s = side * 0.1;
        let (tw, th) = (16.0, 32.0);
        let c = [1.0, 1.0, 1.0, alpha];
        let v = [
            FxVert { pos: head + s, st: [tw, 0.0], col: c },
            FxVert { pos: head - s, st: [0.0, 0.0], col: c },
            FxVert { pos: tail + s * 0.9, st: [tw, th], col: c },
            FxVert { pos: tail - s * 0.9, st: [0.0, th], col: c },
        ];
        // gSPTri2(0, 2, 3, 0, 3, 1)
        Some(FxBatch { kind: FxKind::Beam(tex), verts: vec![v[0], v[2], v[3], v[0], v[3], v[1]] })
    }
}

// ─── sparks (sparks.c) ────────────────────────────────────────────────────────

/// `struct sparktype` — only type 0 (`SPARKTYPE_DEFAULT`) is used by the guns
/// here, plus the green/electrical/tranquilizer bg-hit variants.
#[derive(Clone, Copy, Debug)]
pub struct SparkType {
    pub unk00: u16,
    pub unk02: i16,
    pub unk04: u16,
    pub unk06: u16,
    pub unk08: u16,
    pub unk0a: u16,
    pub weight: f32,
    pub maxage: u16,
    pub unk12: u16,
    pub numsparks: u16,
    pub unk18: u32,
    pub col0: u32,
    pub col1: u32,
    pub decel: f32,
}

/// `g_SparkTypes[]` entries 0 (default), 1 (electrical/blue-white), 0x16
/// (orange bg hit, Farsight), 0x17 (green bg hit), 0x18 (tranquilizer).
pub const SPARKTYPE_DEFAULT: usize = 0;
pub const SPARKTYPE_ELECTRICAL: usize = 1;
pub const SPARKTYPE_PROJECTILE: usize = 0x10;
pub const SPARKTYPE_BGHIT_ORANGE: usize = 0x16;
pub const SPARKTYPE_BGHIT_GREEN: usize = 0x17;
pub const SPARKTYPE_BGHIT_TRANQULIZER: usize = 0x18;

pub fn spark_type(t: usize) -> SparkType {
    let base = SparkType {
        unk00: 100,
        unk02: 28,
        unk04: 100,
        unk06: 1,
        unk08: 0,
        unk0a: 0,
        weight: 2.0,
        maxage: 60,
        unk12: 60,
        numsparks: 15,
        unk18: 1,
        col0: 0xffff80ff,
        col1: 0xffffffff,
        decel: 0.02,
    };
    match t {
        SPARKTYPE_ELECTRICAL => SparkType { col0: 0x80ffffff, ..base },
        SPARKTYPE_PROJECTILE => SparkType { unk00: 50, weight: 1.0, unk12: 30, numsparks: 10, ..base },
        SPARKTYPE_BGHIT_ORANGE => SparkType { maxage: 120, unk12: 120, numsparks: 30, col0: 0xff8080ff, col1: 0xffff80ff, ..base },
        SPARKTYPE_BGHIT_GREEN => SparkType { col0: 0x4fff4fff, ..base },
        SPARKTYPE_BGHIT_TRANQULIZER => SparkType { col0: 0xffff7f7f, ..base },
        _ => base,
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Spark {
    pos: Vec3,
    speed: Vec3,
    ttl: i32,
}

#[derive(Clone, Copy, Debug)]
struct SparkGroup {
    ty: SparkType,
    numsparks: usize,
    age: i32,
    startindex: usize,
    pos: Vec3,
}

pub struct Sparks {
    sparks: Vec<Spark>,
    next: usize,
    groups: Vec<Option<SparkGroup>>,
    next_group: usize,
}

impl Default for Sparks {
    fn default() -> Self {
        Sparks { sparks: vec![Spark::default(); 100], next: 0, groups: vec![None; 10], next_group: 0 }
    }
}

fn rgba(word: u32) -> [f32; 4] {
    [
        ((word >> 24) & 0xff) as f32 / 255.0,
        ((word >> 16) & 0xff) as f32 / 255.0,
        ((word >> 8) & 0xff) as f32 / 255.0,
        (word & 0xff) as f32 / 255.0,
    ]
}

impl Sparks {
    /// Live spark groups.
    pub fn live(&self) -> usize {
        self.groups.iter().flatten().count()
    }

    /// `spark_create` (`sparks.c:67`).
    fn spark_create(&mut self, rng: &mut Rng, pos: Vec3, ty: &SparkType) {
        let i = self.next;
        self.next = (self.next + 1) % self.sparks.len();
        let r = ty.unk00 as u32 * 2 + 1;
        let mut speed = Vec3::new(
            (rng.random() % r) as i32 as f32 - ty.unk00 as f32,
            (rng.random() % r) as i32 as f32 - ty.unk00 as f32,
            (rng.random() % r) as i32 as f32 - ty.unk00 as f32,
        );
        if speed.y == 0.0 {
            speed.y = -0.0001;
        }
        let maxspeed = speed.abs().max_element();
        let len = speed.length();
        if len > 0.0 {
            speed *= maxspeed / len;
        }
        speed.y += (ty.unk00 / 2) as f32;
        speed += pos;
        if speed.y == 0.0 {
            speed.y = -0.0001;
        }
        let ttl = if ty.unk18 % 2 == 1 { (rng.random() % ty.maxage as u32) as i32 } else { ty.maxage as i32 };
        self.sparks[i] = Spark { pos: Vec3::ZERO, speed, ttl };
    }

    /// `sparks_create` (`sparks.c:148`): `dir` is the shot direction, `normal` the
    /// surface normal; the group sprays along the reflection.
    pub fn create(&mut self, rng: &mut Rng, pos: Vec3, dir: Vec3, normal: Vec3, typenum: usize) {
        let ty = spark_type(typenum);
        let gi = self.next_group;
        self.next_group = (self.next_group + 1) % self.groups.len();
        let n = normal.normalize_or_zero();
        let refl = dir + n * (-2.0 * dir.dot(n));
        let l = refl.length();
        let grouppos = refl * (ty.unk02 as f32 / if l == 0.0 { 1.0 } else { l });
        let start = self.next;
        for _ in 0..ty.numsparks {
            // sparkgroup_ensure_free_spark_slot
            for (k, g) in self.groups.iter_mut().enumerate() {
                if k == gi {
                    continue;
                }
                if let Some(grp) = g {
                    if grp.startindex == self.next {
                        grp.startindex = (grp.startindex + 1) % 100;
                        grp.numsparks -= 1;
                        if grp.numsparks == 0 {
                            *g = None;
                        }
                    }
                }
            }
            self.spark_create(rng, grouppos, &ty);
        }
        self.groups[gi] = Some(SparkGroup { ty, numsparks: ty.numsparks as usize, age: 1, startindex: start, pos });
    }

    /// `sparks_tick` (`sparkstick.c:7`).
    pub fn tick(&mut self, lv: Lv) {
        for g in self.groups.iter_mut() {
            let Some(grp) = g else { continue };
            if grp.age >= grp.ty.maxage as i32 {
                *g = None;
                continue;
            }
            for _ in 0..lv.lvupdate60 {
                grp.age += 1;
                let mut idx = grp.startindex;
                for _ in 0..grp.numsparks {
                    let s = &mut self.sparks[idx];
                    if s.ttl != 0 {
                        s.speed.x -= s.speed.x * grp.ty.decel;
                        s.speed.y = (s.speed.y - s.speed.y * grp.ty.decel) - grp.ty.weight;
                        s.speed.z -= s.speed.z * grp.ty.decel;
                        if s.speed.y == 0.0 {
                            s.speed.y = -0.0001;
                        }
                        s.pos += s.speed;
                        s.ttl -= 1;
                    }
                    idx = (idx + 1) % 100;
                }
            }
        }
    }

    /// `sparks_render` (`sparks.c:273`): one stretched triangle per live spark.
    pub fn geometry(&self, campos: Vec3, camlook: Vec3, fovy: f32, xray: Option<&super::xray::Eraser>) -> Option<FxBatch> {
        let look = camlook.abs();
        let axis = if look.y > look.x {
            if look.z > look.y { 2 } else { 1 }
        } else if look.z > look.x {
            2
        } else {
            0
        };
        let mut verts = Vec::new();
        for grp in self.groups.iter().flatten() {
            let ty = &grp.ty;
            let dist = (campos - grp.pos).length();
            if dist > 20000.0 {
                continue;
            }
            let (mut c0, mut c1) = match xray {
                // sparks.c:375
                Some(e) => match e.spark_colour(grp.pos, (ty.col0 & 0xff) as f32 / 255.0) {
                    Some(c) => (c, c),
                    None => continue,
                },
                None => (rgba(ty.col0), rgba(ty.col1)),
            };
            if ty.unk12 < ty.maxage && (ty.unk12 as i32) < grp.age {
                let diff1 = (ty.maxage - ty.unk12) as f32;
                let diff2 = (grp.age - ty.unk12 as i32) as f32;
                let frac = (diff1 - diff2) / diff1;
                c0[3] *= frac;
                c1[3] *= frac;
            }
            let sp120 = dist * 0.2 * (fovy / 60.0);
            let widen = ty.unk06 as f32 + grp.age as f32 * ty.unk0a as f32 + (sp120 as i32) as f32;
            let mut idx = grp.startindex;
            for _ in 0..grp.numsparks {
                let s = &self.sparks[idx];
                idx = (idx + 1) % 100;
                if s.ttl == 0 {
                    continue;
                }
                let sl = s.speed.length();
                let f2 = (sp120 + (ty.unk04 as f32 + grp.age as f32 * ty.unk08 as f32)) / sl;
                let p0 = s.pos;
                let mut p1 = s.pos + s.speed * f2;
                let mut p2 = p1;
                let sp = s.speed.abs();
                let a = match axis {
                    0 => if sp.z > sp.y { 1 } else { 2 },
                    1 => if sp.x > sp.z { 2 } else { 0 },
                    _ => if sp.x > sp.y { 1 } else { 0 },
                };
                p1[a] -= widen;
                p2[a] += widen;
                // Spark-local units are scaled by 0.05 around the group (spd4).
                let w = |p: Vec3| grp.pos + p * 0.05;
                verts.push(FxVert { pos: w(p0), st: [4.0, -8.0], col: c1 });
                verts.push(FxVert { pos: w(p1), st: [-1.0, 15.5], col: c0 });
                verts.push(FxVert { pos: w(p2), st: [9.0, 15.5], col: c0 });
            }
        }
        (!verts.is_empty()).then_some(FxBatch { kind: FxKind::Spark, verts })
    }
}

// ─── wallhits (wallhit.c) ─────────────────────────────────────────────────────

/// `g_WallhitTexes[]` (`wallhit.c:49`): half-extent (cm) and type.
pub const WALLHITTEX_BULLET1: usize = 0x01;
pub const WALLHITTEX_BULLET2: usize = 0x06;
pub const WALLHITTEX_SCORCH: usize = 0x07;
const WALLHIT_SIZE: [f32; 18] = [10.0, 6.0, 8.0, 6.0, 8.0, 12.0, 6.0, 100.0, 24.0, 20.0, 20.0, 20.0, 20.0, 6.0, 8.0, 12.0, 4.0, 6.0];
#[derive(Clone, Copy, PartialEq, Eq)]
enum WallhitType {
    Bullet,
    Soft,
    Scorch,
    Paint,
    Blood,
}
const WALLHIT_TYPE: [WallhitType; 18] = {
    use WallhitType::*;
    [Bullet, Bullet, Soft, Bullet, Bullet, Bullet, Bullet, Scorch, Paint, Blood, Blood, Blood, Blood, Bullet, Bullet, Bullet, Bullet, Bullet]
};
/// `g_TcWallhitConfigs[]` texture numbers and sizes.
pub const WALLHIT_TEX: [(u16, f32, f32); 18] = [
    (0x0003, 48.0, 48.0),
    (0x0c27, 64.0, 64.0),
    (0x0da5, 64.0, 48.0),
    (0x0003, 48.0, 48.0),
    (0x0003, 48.0, 48.0),
    (0x0003, 48.0, 48.0),
    (0x0004, 32.0, 32.0),
    (0x0005, 54.0, 54.0),
    (0x0c28, 64.0, 64.0),
    (0x0854, 48.0, 48.0),
    (0x0855, 48.0, 48.0),
    (0x0856, 48.0, 48.0),
    (0x08f0, 24.0, 24.0),
    (0x0b53, 64.0, 64.0),
    (0x0b53, 64.0, 64.0),
    (0x0b53, 64.0, 64.0),
    (0x0d74, 32.0, 24.0),
    (0x0d72, 32.0, 24.0),
];

#[derive(Clone, Debug)]
pub struct Wallhit {
    pub corners: [Vec3; 4],
    pub texnum: usize,
    /// `finalcolours[4]`: one per corner.
    pub cols: [[f32; 4]; 4],
}

/// `g_MaxBgWallhitsPerRoom`-style cap for the one-room range.
pub const MAX_WALLHITS: usize = 80;

/// `wallhit_create` (`wallhit.c:637`): a 0.6–0.7 scale of the texture's size.
pub fn wallhit_create(rng: &mut Rng, pos: Vec3, normal: Vec3, gunpos: Vec3, texnum: usize, brightness: f32) -> Wallhit {
    let scale = rng.randomfrac() * 0.1 + 0.6;
    let width = WALLHIT_SIZE[texnum] * scale;
    let height = WALLHIT_SIZE[texnum] * scale;
    wallhit_create_with_20_args(rng, pos, normal, Some(gunpos), texnum, width, height, 0xff, 0xff, 0, brightness)
}

/// `wallhit_create_with_20_args` (`wallhit.c:652`) for a background hit: a quad
/// on the surface oriented from the normal, flipped to face `arg2` (the gun or
/// the blast), with per-corner colours by type × `room_get_final_brightness_for_player`.
#[allow(clippy::too_many_arguments)]
pub fn wallhit_create_with_20_args(
    rng: &mut Rng,
    pos: Vec3,
    normal: Vec3,
    arg2: Option<Vec3>,
    texnum: usize,
    width: f32,
    height: f32,
    minalpha: u8,
    maxalpha: u8,
    rotdeg: u32,
    brightness: f32,
) -> Wallhit {
    let n = normal.normalize_or_zero();
    // NTSC_1_0+: BULLET2/blood/bpglass/METAL keep the given rotdeg; the rest spin.
    let rotdeg = match texnum {
        0x06 | 0x09..=0x0f | 0x11 => rotdeg,
        _ => rng.random() % 360,
    };
    let eps = 1e-6;
    let (xz, yz, zz) = (normal.x.abs() < eps, normal.y.abs() < eps, normal.z.abs() < eps);
    let (mut u, mut v);
    if xz && zz {
        u = Vec3::new(-1.0, 0.0, 0.0);
        v = Vec3::new(0.0, 0.0, if normal.y >= 0.0 { -1.0 } else { 1.0 });
    } else if xz && yz {
        u = Vec3::new(if normal.z >= 0.0 { 1.0 } else { -1.0 }, 0.0, 0.0);
        v = Vec3::new(0.0, -1.0, 0.0);
    } else if yz && zz {
        u = Vec3::new(0.0, if normal.x >= 0.0 { -1.0 } else { 1.0 }, 0.0);
        v = Vec3::new(0.0, 0.0, 1.0);
    } else {
        let f0 = (n.x * n.x + n.z * n.z).sqrt();
        let (xv, zv) = (n.x / f0, n.z / f0);
        u = Vec3::new(zv, 0.0, -xv);
        v = Vec3::new(n.y * xv, -f0, n.y * zv);
    }
    if rotdeg != 0 {
        let (s, c) = (rotdeg as f32 * 0.017_453_292).sin_cos();
        let (u0, v0) = (u, v);
        u = u0 * c + v0 * s;
        v = u0 * -s + v0 * c;
    }
    // The source is on the far side of the plane → flip v (the "sum < 0" test).
    if let Some(src) = arg2 {
        if (src - pos).dot(n) < 0.0 {
            v = -v;
        }
    }
    let u = u * width;
    let v = v * height;
    let c0 = u + v;
    let c1 = u - v;
    let corners = [pos + c0, pos + c1, pos - c0, pos + (v - u)];
    let frac = brightness / 255.0;
    let range = maxalpha as u32 - minalpha as u32;
    let alpha = if range != 0 { minalpha as u32 + rng.random() % range } else { 0 };
    let ty = WALLHIT_TYPE[texnum];
    let mut cols = [[0.0f32; 4]; 4];
    for c in cols.iter_mut() {
        let (g, a) = match ty {
            WallhitType::Bullet => (255 - rng.random() % 40, if alpha != 0 { alpha } else { 255 }),
            WallhitType::Soft => {
                let g = rng.random() % 70;
                (g, if alpha != 0 { alpha } else { 255 - rng.random() % 50 })
            }
            WallhitType::Scorch => {
                let g = rng.random() % 50;
                (g, if alpha != 0 { alpha } else { 255 - rng.random() % 80 })
            }
            WallhitType::Paint | WallhitType::Blood => (255, 255),
        };
        let g = ((g as f32 * frac) as u32 & 0xff) as f32 / 255.0;
        *c = [g, g, g, a as f32 / 255.0];
    }
    Wallhit { corners, texnum, cols }
}

impl Wallhit {
    pub fn tris(&self, out: &mut Vec<FxVert>) {
        let (_, tw, th) = WALLHIT_TEX[self.texnum];
        let st = [[0.0, th], [0.0, 0.0], [tw, 0.0], [tw, th]];
        let v = |i: usize| FxVert { pos: self.corners[i], st: st[i], col: self.cols[i] };
        out.extend_from_slice(&[v(0), v(1), v(2), v(0), v(2), v(3)]);
    }
}

// ─── casings (gunfx.c / casingtick.c) ─────────────────────────────────────────

/// `g_CartFileNums` by `casingeject`.
pub const CART_MODELS: [&str; 4] = ["cartridge", "cartrifle", "cartblue", "cartshell"];

#[derive(Clone, Debug)]
pub struct Casing {
    pub model: usize,
    pub pos: Vec3,
    pub speed: Vec3,
    pub rot: Mat3,
    pub rotspeed: Mat3,
    pub ground: f32,
}

/// `casing_create_for_hand` (`gunfx.c:662`). `mtx` is the cart-eject node's world
/// matrix; `handvel` the hand's world displacement this frame / lvupdate.
pub fn casing_create_for_hand(
    rng: &mut Rng,
    weaponnum: i32,
    casingtype: i32,
    ground: f32,
    mtx: &Mat4,
    handvel: Vec3,
) -> Option<Casing> {
    if casingtype < 0 || casingtype as usize >= CART_MODELS.len() {
        return None;
    }
    let pos = mtx.w_axis.truncate();
    let rot = Mat3::from_mat4(*mtx);
    let rand_rot = |rng: &mut Rng, div: f32, off: f32| -> Mat3 {
        let a = Vec3::new(
            2.0 * rng.randomfrac() * baddtor(360.0) * div - off,
            2.0 * rng.randomfrac() * baddtor(360.0) * div - off,
            2.0 * rng.randomfrac() * baddtor(360.0) * div - off,
        );
        Mat3::from_mat4(pdmtx::load_rotation(a))
    };
    let mut speed;
    let rotspeed;
    if matches!(weaponnum, WEAPON_PP9I | WEAPON_CC13 | WEAPON_FALCON2 | WEAPON_MAGSEC4) {
        speed = Vec3::new(
            -(rng.randomfrac() * 0.533_333_3 * (1.0 / 16.0) + 0.533_333_3),
            rng.randomfrac() * 2.5 * (1.0 / 16.0) + 2.5,
            0.0,
        );
        speed = pdmtx::rotate(mtx, speed);
        rotspeed = rand_rot(rng, 1.0 / 16.0, baddtor(22.5));
    } else {
        speed = if weaponnum == WEAPON_REAPER {
            Vec3::new(-(rng.randomfrac() * 0.416_666_66 * 0.125 + 0.416_666_66), rng.randomfrac() * 3.333_333_3 * 0.125 + 3.333_333_3, 0.0)
        } else {
            Vec3::new(-(rng.randomfrac() * 1.416_666_6 * 0.125 + 1.416_666_6), rng.randomfrac() * 1.666_666_6 * 0.125 + 1.666_666_6, 0.0)
        };
        if weaponnum == WEAPON_DY357MAGNUM || weaponnum == WEAPON_DY357LX {
            speed = Vec3::new(0.0, 0.0, -1.0);
        }
        speed = pdmtx::rotate(mtx, speed);
        if weaponnum == WEAPON_REAPER {
            let r = rand_rot(rng, 1.0 / 64.0, 0.098_159_14);
            speed = r * speed;
        }
        rotspeed = rand_rot(rng, 1.0 / 64.0, 0.098_159_14);
    }
    // The sub-frame head start (`f0` = a random fraction of a frame, in frames).
    let magic: u32 = 0x15aca6;
    let sp5c = (((rng.random() >> 24).wrapping_mul(magic) as i32) >> 10) as u32 + magic;
    let f0 = (rng.random() % sp5c) as f32 / (46_875_000.0 / 60.0);
    let newyspeed = speed.y - f0 * 0.277_777_8;
    let mut pos = pos;
    pos.y += f0 * (speed.y + newyspeed) * 0.5;
    pos.x += f0 * speed.x;
    pos.z += f0 * speed.z;
    speed.y = newyspeed;
    speed += handvel;
    Some(Casing { model: casingtype as usize, pos, speed, rot, rotspeed, ground })
}

impl Casing {
    /// `casing_tick` (`casingtick.c:13`). Returns true when it hits the ground
    /// (and is removed — the caller plays `SFXMAP_8051` at 0.98..1.23 pitch).
    pub fn tick(&mut self, lv: Lv) -> bool {
        let l = lv.lvupdate60freal;
        let tmp = self.speed.y - l * (1.0 / 3.6);
        self.pos.y += l * 0.5 * (self.speed.y + tmp);
        if self.pos.y < self.ground {
            return true;
        }
        self.speed.y = tmp;
        self.pos.x += l * self.speed.x;
        self.pos.z += l * self.speed.z;
        for _ in 0..lv.lvupdate240 {
            self.rot = self.rotspeed * self.rot;
        }
        false
    }

    /// `casing_render`'s model matrix (world, before world→camera).
    pub fn world_matrix(&self) -> Mat4 {
        let mut m = Mat4::from_mat3(self.rot);
        pdmtx::scale3(&mut m, 0.1);
        pdmtx::set_translation(&mut m, self.pos);
        m
    }
}
