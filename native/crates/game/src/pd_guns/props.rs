//! The world objects the guns spawn — grenades, N-Bombs, mines, knives, rockets,
//! bolts, grenade rounds, the Laptop sentry — ported from `propobj.c`
//! (`projectile_launch`, `projectile_tick`, `projectile_settle`, `obj_stick`,
//! `weapon_tick`, `prop_explode`) and `projectile.c` (NTSC final).
//!
//! Substituted: PD's collision (`cd_*`, `bg_test_hit_in_room`,
//! `projectile_find_colliding_prop`) is the range's boxes ([`Range::raycast`],
//! [`Range::floor_below`]); the target boards stand in for objects. There are no
//! chrs, so the chr branches (embedding in bodies, shields, homing targets) have
//! nothing to hit.

use std::sync::Arc;

use glam::{Mat3, Mat4, Quat, Vec3};

use super::bgun::Lv;
use super::explosions::{self, Explosions};
use super::gset::*;
use super::model::ModelDef;
use super::pdmtx;
use super::range::{HitKind, Range};
use super::smoke::{self, Smokes};
use crate::pd_spike::pdmath::{baddtor, Rng};

pub const WEAPON_ROCKET: i32 = 0x53;
pub const WEAPON_HOMINGROCKET: i32 = 0x54;
pub const WEAPON_GRENADEROUND: i32 = 0x55;
pub const WEAPON_BOLT: i32 = 0x56;
pub const FUNC_2: usize = 2;

pub const PROJECTILEFLAG_AIRBORNE: u32 = 0x0000_0001;
pub const PROJECTILEFLAG_FORCEGOODBOUNCE: u32 = 0x0000_0002;
pub const PROJECTILEFLAG_STICKY: u32 = 0x0000_0004;
pub const PROJECTILEFLAG_POWERED: u32 = 0x0000_0010;
pub const PROJECTILEFLAG_MISSILE: u32 = 0x0000_0020;
pub const PROJECTILEFLAG_LAUNCHING: u32 = 0x0000_0080;
pub const PROJECTILEFLAG_BOUNCEKEEPROT: u32 = 0x0000_0100;
pub const PROJECTILEFLAG_SETTLING: u32 = 0x0000_0400;
pub const PROJECTILEFLAG_NOTIMELIMIT: u32 = 0x0000_4000;
pub const PROJECTILEFLAG_LIGHTWEIGHT: u32 = 0x4000_0000;

/// `g_ModelStates[].scale` for every projectile model: 0x0199 / 4096
/// (`modeldata/general.c:404`, applied by `obj_init`, `propobj.c:2098`).
pub const PROP_MODEL_SCALE: f32 = 409.0 / 4096.0;

/// The projectile models by `projectilemodelnum` (`MODEL_*` → `FILE_P*`).
pub fn projectile_model_stem(modelnum: i32) -> Option<&'static str> {
    Some(match modelnum {
        0x0ff => "chrdragon",
        0x10f => "chrknife",
        0x110 => "chrnbomb",
        0x112 => "chrgrenade",
        0x113 => "chrtimedmine",
        0x114 => "chrproximitymine",
        0x115 => "chrremotemine",
        0x11f => "chrdyrocketmis",
        0x120 => "chrskrocketmis",
        0x121 => "chrcrossbolt",
        0x122 => "chrdevgrenade",
        0x123 => "chrdraggrenade",
        0x157 => "chrautogun",
        _ => return None,
    })
}

/// `struct modelrodata_bbox`.
#[derive(Clone, Copy, Debug)]
pub struct Bbox {
    pub xmin: f32,
    pub xmax: f32,
    pub ymin: f32,
    pub ymax: f32,
    pub zmin: f32,
    pub zmax: f32,
}

impl Bbox {
    pub fn from_def(def: &ModelDef) -> Self {
        let b = def.bbox.unwrap_or([-10.0, 10.0, -10.0, 10.0, -10.0, 10.0]);
        Bbox { xmin: b[0], xmax: b[1], ymin: b[2], ymax: b[3], zmin: b[4], zmax: b[5] }
    }

    /// `obj_get_rotated_local_min` (`propobj.c:406`).
    fn rotated_local_min(&self, a1: f32, a2: f32, a3: f32) -> f32 {
        let mut sum = 0.0;
        sum += if a1 >= 0.0 { self.xmin * a1 } else { self.xmax * a1 };
        sum += if a2 >= 0.0 { self.ymin * a2 } else { self.ymax * a2 };
        sum += if a3 >= 0.0 { self.zmin * a3 } else { self.zmax * a3 };
        sum
    }

    /// `obj_get_rotated_local_y_min_by_mtx3` (`propobj.c:384`).
    pub fn rotated_y_min(&self, r: &Mat3) -> f32 {
        self.rotated_local_min(r.x_axis.y, r.y_axis.y, r.z_axis.y)
    }
}

/// `struct projectile` — the fields the ported paths use.
#[derive(Clone, Debug)]
pub struct Projectile {
    pub flags: u32,
    pub speed: Vec3,
    pub accel: Vec3,
    /// The per-240-tick spin (`projectile->mtx`), a pure rotation.
    pub mtx: Mat3,
    /// `ownerprop != NULL`: the thrower's own perimeter is off while it ticks.
    pub has_owner: bool,
    pub bouncecount: i32,
    pub bounceframe: i32,
    pub collisionframe: i32,
    pub losttimer240: i32,
    pub flighttime240: i32,
    pub powerlimit240: i32,
    pub pickuptimer240: i32,
    pub hitspeedpreservationfrac: f32,
    pub speeddecel: f32,
    pub missileyaccel: f32,
    pub nextsteppos: Vec3,
    pub settledrotfrac: f32,
    pub settledrotinc: f32,
    pub unk068: Quat,
    pub unk078: Quat,
    pub unk0b8: [f32; 3],
    pub lastwooshframe: i32,
    pub startframe: i32,
}

impl Default for Projectile {
    fn default() -> Self {
        Projectile {
            flags: 0,
            speed: Vec3::ZERO,
            accel: Vec3::ZERO,
            mtx: Mat3::IDENTITY,
            has_owner: false,
            bouncecount: 0,
            bounceframe: -1,
            collisionframe: -10,
            losttimer240: 0,
            flighttime240: 0,
            powerlimit240: -1,
            pickuptimer240: 0,
            hitspeedpreservationfrac: 0.0,
            speeddecel: 0.0,
            missileyaccel: 0.0,
            nextsteppos: Vec3::ZERO,
            settledrotfrac: 1.0,
            settledrotinc: 0.0,
            unk068: Quat::IDENTITY,
            unk078: Quat::IDENTITY,
            unk0b8: [1.0; 3],
            lastwooshframe: -100,
            startframe: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjType {
    Weapon,
    Autogun,
}

/// A `weaponobj` / `autogunobj` + its prop.
pub struct WorldObj {
    pub id: u32,
    pub ty: ObjType,
    pub weaponnum: i32,
    pub gunfunc: usize,
    pub timer240: i32,
    pub def: Arc<ModelDef>,
    pub bbox: Bbox,
    /// `model->scale`.
    pub scale: f32,
    pub pos: Vec3,
    /// `obj->realrot` (the model scale folded in, as PD's `obj_place` does).
    pub realrot: Mat3,
    pub proj: Option<Projectile>,
    /// `OBJHFLAG_ATTACHED`: stuck to the background or a prop.
    pub attached: bool,
    /// Stuck into this board (`obj_embed`).
    pub embedded_board: Option<usize>,
    pub thrownknife: bool,
    /// `OBJHFLAG_DELETING`.
    pub deleting: bool,
    /// `prop_set_dangerous`.
    pub dangerous: bool,
    pub settlerot_byactualsize: bool,
    pub settlerot_laptop: bool,
    /// `OBJFLAG2_THROWTHROUGH` (a rocket still in the hand).
    pub throwthrough: bool,
    /// `OBJFLAG_HELDROCKET`.
    pub heldrocket: bool,
    /// `obj->hidden >> 28`: the owning player.
    pub owner: i32,
    /// `autogunobj` state (the Laptop sentry).
    pub autogun: Option<super::autogun::Autogun>,
}

impl WorldObj {
    /// The model's root matrix in world space (`realrot` + `pos`).
    pub fn root_matrix(&self) -> Mat4 {
        let mut m = Mat4::from_mat3(self.realrot);
        pdmtx::set_translation(&mut m, self.pos);
        m
    }

    /// `obj_init_matrices` (`propobj.c:10898`) in world space: joint 0 is the
    /// object's own transform — no root-node offset — and, for weapons,
    /// `weapon_init_matrices` leaves the rest identity.
    pub fn init_matrices(&self) -> Vec<Mat4> {
        let mut out = vec![Mat4::IDENTITY; self.def.nummatrices.max(1)];
        out[0] = self.root_matrix();
        if let Some(a) = &self.autogun {
            a.init_matrices(&self.def, &mut out);
        }
        out
    }

    fn gset_flags(&self, gset: &Gset) -> u32 {
        // invitem_grenaderound borrows the Devastator's functions (invitems.c:5670).
        let w = if self.weaponnum == WEAPON_GRENADEROUND { WEAPON_DEVASTATOR } else { self.weaponnum };
        gset.func(w, self.gunfunc).map_or(0, |f| f.flags)
    }
}

/// Side effects the world layer applies (sounds and sparks it owns).
#[derive(Default, Debug)]
pub struct ObjOut {
    /// `ps_create(prop, sound)`: (sound, position, pitch).
    pub sounds: Vec<(u16, Vec3, f32)>,
    /// `bgun_play_bg_hit_sound(&weapon->gset, pos)`.
    pub bg_hit_sounds: Vec<(i32, Vec3)>,
    /// `sparks_create(pos, dir, normal, type)`.
    pub sparks: Vec<(Vec3, Vec3, Vec3, usize)>,
    /// A board took a hit or damage: (board, damage).
    pub board_hits: Vec<(usize, f32)>,
    /// `nbomb_create_storm(pos)`.
    pub nbombs: Vec<Vec3>,
}

/// Everything a tick reads or writes besides the object itself.
pub struct ObjCtx<'a> {
    pub rng: &'a mut Rng,
    pub lv: Lv,
    pub gset: &'a Gset,
    pub world: &'a Range,
    pub smokes: &'a mut Smokes,
    pub explosions: &'a mut Explosions,
    pub campos: Vec3,
    pub lodscalez: f32,
    /// `g_Vars.currentplayer->prop->pos`.
    pub playerpos: Vec3,
    /// `g_PlayersDetonatingMines & (1 << 0)`.
    pub detonating: bool,
    pub brightness: f32,
    pub out: &'a mut ObjOut,
}

/// A segment cast against the range: hit position, surface normal, the board.
struct SegHit {
    pos: Vec3,
    normal: Vec3,
    board: Option<usize>,
}

fn cast(world: &Range, from: Vec3, to: Vec3) -> Option<SegHit> {
    let d = to - from;
    let len = d.length();
    if len <= 0.0 {
        return None;
    }
    let hit = world.raycast(from, d / len, len)?;
    let board = match hit.kind {
        HitKind::Target(i) => Some(i),
        HitKind::World => None,
    };
    Some(SegHit { pos: hit.pos, normal: hit.normal, board })
}

/// `projectile_load_random_rotation` (`projectile.c:14`): up to ±1.4° a quarter-tick.
pub fn projectile_load_random_rotation(rng: &mut Rng) -> Mat3 {
    let r = |rng: &mut Rng| rng.randomfrac() * baddtor(360.0) * (1.0 / 128.0) - baddtor(180.0) / 128.0;
    let rot = Vec3::new(r(rng), r(rng), r(rng));
    Mat3::from_mat4(pdmtx::load_rotation(rot))
}

/// PD's `atan2f(x, z)` (`atan2f.c`): the angle from +z towards +x, 0..2π.
pub fn pd_atan2f(x: f32, z: f32) -> f32 {
    let a = x.atan2(z);
    if a < 0.0 {
        a + std::f32::consts::TAU
    } else {
        a
    }
}

/// `func0f06e9cc` (`propobj.c:3943`): the orientation that stands an object's
/// local +y on the surface normal `n`.
pub fn func0f06e9cc(n: Vec3) -> Mat4 {
    let f0 = n.length();
    let (x, y, z) = (n.x / f0, n.y / f0, n.z / f0);
    let (sp124, sp120, sp11c, sp118, sp114);
    if x == 0.0 && z == 0.0 {
        sp124 = 0.0;
        sp120 = 0.0;
        sp11c = y;
        sp118 = 1.0;
        sp114 = 0.0;
    } else {
        let a = (x * x + z * z).sqrt();
        let b = x / a;
        sp118 = z / a;
        sp114 = -b;
        sp124 = y * b;
        sp120 = -a;
        sp11c = y * sp118;
    }
    let spf4 = pd_atan2f(sp118, sp114);
    let spb0 = pdmtx::load_y_rotation(-spf4);
    let sp24 = pdmtx::rotate(&spb0, Vec3::new(sp124, sp120, sp11c));
    let spf0 = pd_atan2f(sp24.x, sp24.y);
    let sp70 = pdmtx::load_y_rotation(baddtor(-90.0) + spf4);
    let sp30 = pdmtx::load_x_rotation(baddtor(-90.0) - spf0);
    pdmtx::mul(&sp70, &sp30)
}

/// `func0f06cd00` (`propobj.c:3256`) for a STICKY projectile: a segment from
/// the prop to `pos` against the world and the boards. On a hit, `arg2` is the
/// hit backed off 0.1 cm and `arg3` the normal. Returns (collided, board hit).
fn func0f06cd00(o: &WorldObj, world: &Range, pos: Vec3, arg2: &mut Vec3, arg3: &mut Vec3) -> (bool, Option<usize>) {
    let sticky = o.proj.as_ref().is_some_and(|p| p.flags & PROJECTILEFLAG_STICKY != 0);
    if o.pos == pos || !sticky {
        return (false, None);
    }
    let Some(hit) = cast(world, o.pos, pos) else { return (false, None) };
    *arg2 = hit.pos;
    *arg3 = hit.normal;
    let d = pos - o.pos;
    let distance = d.length();
    let mult = if distance > 0.1 { 0.1 / distance } else { 0.5 };
    *arg2 -= d * mult;
    if *arg3 != Vec3::ZERO {
        *arg3 = arg3.normalize();
    } else {
        *arg3 = Vec3::Z;
    }
    (true, hit.board)
}

/// `func0f06d37c` (`propobj.c:3401`): move a non-sticky object as a 10 cm
/// cylinder (`obj_get_radius`). Substituted: a blocked move doesn't happen at
/// all (as PD's) and the normal is the box face's.
fn func0f06d37c(o: &mut WorldObj, world: &Range, to: Vec3, arg3: &mut Vec3) -> bool {
    if o.pos == to {
        return true;
    }
    let radius = 10.0;
    for s in world.solids.iter() {
        let lo = s.min - Vec3::new(radius, 0.0, radius);
        let hi = s.max + Vec3::new(radius, 0.0, radius);
        if to.x > lo.x && to.x < hi.x && to.z > lo.z && to.z < hi.z && to.y > s.min.y && to.y < s.max.y {
            let dx = (to.x - lo.x).min(hi.x - to.x);
            let dz = (to.z - lo.z).min(hi.z - to.z);
            *arg3 = if dx < dz {
                Vec3::new(if to.x - lo.x < hi.x - to.x { -1.0 } else { 1.0 }, 0.0, 0.0)
            } else {
                Vec3::new(0.0, 0.0, if to.z - lo.z < hi.z - to.z { -1.0 } else { 1.0 })
            };
            return false;
        }
    }
    let b = world.bounds;
    let clamped = Vec3::new(to.x.clamp(b.min.x + radius, b.max.x - radius), to.y, to.z.clamp(b.min.z + radius, b.max.z - radius));
    if clamped.x != to.x || clamped.z != to.z {
        *arg3 = if clamped.x != to.x { Vec3::new(-(to.x - clamped.x).signum(), 0.0, 0.0) } else { Vec3::new(0.0, 0.0, -(to.z - clamped.z).signum()) };
        return false;
    }
    o.pos = to;
    true
}

/// The ground under a falling object: the highest box top at or below where
/// it was last frame (`cd_find_ceiling_room_at_pos_ycfn` for the range).
fn ground_under(world: &Range, x: f32, z: f32, from_y: f32) -> Option<(f32, Vec3)> {
    world.floor_below(Vec3::new(x, from_y, z)).map(|(y, n, _)| (y, n))
}

/// `projectile_launch` (`propobj.c:6230`): the first step, from where the
/// projectile was placed to its `nextsteppos` (the muzzle).
pub fn projectile_launch(o: &mut WorldObj, c: &mut ObjCtx, arg2: &mut Vec3, arg3: &mut Vec3) -> bool {
    let Some(next) = o.proj.as_ref().map(|p| p.nextsteppos) else { return false };
    let (hit, _) = func0f06cd00(o, c.world, next, arg2, arg3);
    if !hit {
        o.pos = next;
    } else if o.ty == ObjType::Weapon && (o.weaponnum == WEAPON_ROCKET || o.weaponnum == WEAPON_HOMINGROCKET) {
        o.timer240 = 0;
        o.pos = *arg2;
    }
    if let Some(p) = o.proj.as_mut() {
        p.flags &= !PROJECTILEFLAG_LAUNCHING;
    }
    hit
}

/// `projectile_tick` (`propobj.c:6279`), the AIRBORNE and SETTLING branches
/// (SLIDING is furniture pushing — nothing in the range slides).
pub fn projectile_tick(o: &mut WorldObj, c: &mut ObjCtx) -> bool {
    let lv = c.lv;
    if lv.lvupdate240 <= 0 || o.proj.is_none() {
        return false;
    }
    let mut moved = false;
    o.attached = false;
    let mut sp5e8 = Vec3::ZERO;
    let mut sp5f4 = Vec3::ZERO;
    if o.proj.as_ref().unwrap().flags & PROJECTILEFLAG_LAUNCHING != 0 {
        projectile_launch(o, c, &mut sp5e8, &mut sp5f4);
    }
    let mut sp5dc = o.pos;
    {
        let p = o.proj.as_mut().unwrap();
        if p.pickuptimer240 > 0 {
            p.pickuptimer240 -= lv.lvupdate240;
        }
    }
    let flags = o.proj.as_ref().unwrap().flags;
    if flags & PROJECTILEFLAG_AIRBORNE != 0 {
        moved = tick_airborne(o, c, sp5dc, sp5e8, sp5f4);
    } else if flags & PROJECTILEFLAG_SETTLING != 0 {
        moved = tick_settling(o, c, &mut sp5dc);
    }
    moved
}

fn tick_airborne(o: &mut WorldObj, c: &mut ObjCtx, mut sp5dc: Vec3, mut sp5e8: Vec3, mut sp5f4: Vec3) -> bool {
    let lv = c.lv;
    let lv60 = lv.lvupdate60freal;
    let mut settle = false;
    let mut atground = false;
    let mut handled = false;
    {
        let p = o.proj.as_mut().unwrap();
        // Lost for 40 seconds, or out of the world: delete.
        p.losttimer240 += lv.lvupdate240;
        if (p.flags & PROJECTILEFLAG_NOTIMELIMIT == 0 && p.losttimer240 > 40 * 240)
            || o.pos.y < -20000.0
            || o.pos.y > 32000.0
            || o.pos.x.abs() > 32000.0
            || o.pos.z.abs() > 32000.0
        {
            o.deleting = true;
        }
        p.flighttime240 += lv.lvupdate240;
    }
    let realrot = o.realrot;
    // Homing rockets steer at `targetprop` (the player's tracked prop). The
    // range has no trackable chrs, so a homing rocket flies on its launch line.
    {
        let p = o.proj.as_mut().unwrap();
        if p.flags & PROJECTILEFLAG_POWERED == 0 {
            p.speed.y += (p.accel.y + p.missileyaccel) * lv60;
            let fallspeed = if p.flags & PROJECTILEFLAG_LIGHTWEIGHT != 0 {
                p.speed.y - (1.0 / 7.2) * lv60
            } else {
                p.speed.y - (1.0 / 3.6) * lv60
            };
            sp5dc.y += lv60 * (p.speed.y + fallspeed) * 0.5;
            p.speed.y = fallspeed;
        } else {
            p.speed.y += (p.accel.y + p.missileyaccel) * lv60;
            sp5dc.y += p.speed.y * lv60;
        }
        p.speed.x += p.accel.x * lv60;
        p.speed.z += p.accel.z * lv60;
        sp5dc.x += p.speed.x * lv60;
        sp5dc.z += p.speed.z * lv60;
        // projectile_update_matrix (`projectile.c:49`): spin once per quarter-tick.
        for _ in 0..lv.lvupdate240 {
            o.realrot = p.mtx * o.realrot;
        }
    }
    let prevpos = o.pos;
    let sticky = o.proj.as_ref().unwrap().flags & PROJECTILEFLAG_STICKY != 0;
    let (mut collided, hitboard) = if sticky {
        func0f06cd00(o, c.world, sp5dc, &mut sp5e8, &mut sp5f4)
    } else {
        let ok = func0f06d37c(o, c.world, sp5dc, &mut sp5f4);
        if !ok {
            sp5e8 = o.pos;
        }
        (!ok, None)
    };
    let moved = true;

    if sticky {
        if collided {
            let mut stick = false;
            match o.ty {
                // Thrown laptops stick to the background, not props.
                ObjType::Autogun => stick = hitboard.is_none(),
                ObjType::Weapon => {
                    if matches!(o.weaponnum, WEAPON_REMOTEMINE | WEAPON_TIMEDMINE | WEAPON_PROXIMITYMINE | WEAPON_BOLT | WEAPON_COMBATKNIFE)
                        || o.gset_flags(c.gset) & FUNCFLAG_STICKTOWALL != 0
                    {
                        stick = true;
                        if o.weaponnum == WEAPON_GRENADEROUND && o.gunfunc == FUNC_SECONDARY {
                            if o.timer240 == 1 {
                                stick = false;
                                o.timer240 = 0;
                            } else {
                                o.timer240 = 480;
                            }
                        }
                    }
                }
            }
            if o.ty == ObjType::Weapon {
                if let Some(b) = hitboard {
                    match o.weaponnum {
                        // A bolt or knife in a target: PD scores it (fr_calculate_hit).
                        WEAPON_BOLT | WEAPON_COMBATKNIFE => c.out.board_hits.push((b, 0.0)),
                        // A rocket into an object: obj_damage(100), boom.
                        WEAPON_ROCKET | WEAPON_HOMINGROCKET => {
                            c.out.board_hits.push((b, 100.0));
                            handled = true;
                            o.timer240 = 0;
                        }
                        _ => {}
                    }
                }
            }
            if !handled && stick {
                handled = true;
                if o.ty == ObjType::Weapon && (o.weaponnum == WEAPON_BOLT || o.weaponnum == WEAPON_COMBATKNIFE) {
                    let dir = o.proj.as_ref().unwrap().speed.normalize_or_zero();
                    c.out.sparks.push((sp5e8, dir, sp5f4, super::fx::SPARKTYPE_PROJECTILE));
                }
                obj_stick(o, c, sp5e8, sp5f4, hitboard);
            }
        }
        if !handled {
            if !collided {
                o.pos = sp5dc;
            } else {
                sp5dc = sp5e8;
                o.pos = sp5dc;
            }
        }
    }

    if !handled {
        let sp37c = o.bbox.rotated_y_min(&o.realrot);
        let sp5ac = Vec3::new(o.pos.x, o.pos.y + sp37c, o.pos.z);
        // The floor under the object, and whether its bottom crossed it this
        // tick (PD: a floor-only LOS test from prevpos to the bottom point).
        match ground_under(c.world, o.pos.x, o.pos.z, prevpos.y.max(o.pos.y)) {
            Some((sp390, n)) => {
                if o.pos.y + sp37c < sp390 && prevpos.y + sp37c >= sp390 - 0.5 {
                    settle = true;
                    sp5f4 = n.normalize();
                    sp5e8 = Vec3::new(o.pos.x, sp390, o.pos.z);
                    collided = true;
                }
            }
            None => {
                // Out of the room: back to where it was, stop drifting.
                o.pos = prevpos;
                let p = o.proj.as_mut().unwrap();
                p.speed.x = 0.0;
                p.speed.z = 0.0;
            }
        }
        let _ = sp5ac;

        if collided {
            let lvframe60 = lv.lvframe60;
            {
                let p = o.proj.as_mut().unwrap();
                if (p.speed.y <= 0.0 && prevpos.y <= o.pos.y) || (p.flags & PROJECTILEFLAG_STICKY == 0 && settle) {
                    atground = true;
                }
                if p.hitspeedpreservationfrac > 0.0 {
                    let f0 = p.speed.dot(sp5f4) * -(p.hitspeedpreservationfrac + 1.0);
                    let oldyspeed = p.speed.y;
                    p.speed += sp5f4 * f0;
                    if oldyspeed <= 0.0 && p.speed.y >= 0.0 {
                        atground = true;
                    }
                }
            }
            if o.ty == ObjType::Weapon && o.weaponnum == WEAPON_GRENADE && o.gunfunc == FUNC_SECONDARY && o.proj.as_ref().unwrap().hitspeedpreservationfrac > 0.0 {
                c.smokes.smoke_create_at_prop(o.id, o.pos, smoke::SMOKETYPE_PINBALL);
            }
            if atground {
                o.pos.y = sp5e8.y - sp37c;
                if settle {
                    o.pos.y += obj_get_ground_clearance(o);
                }
            }
            let newmtx;
            {
                let p = o.proj.as_mut().unwrap();
                newmtx = p.flags & PROJECTILEFLAG_BOUNCEKEEPROT == 0 && (p.bounceframe < 0 || p.bounceframe < lvframe60 - 60);
            }
            if newmtx {
                let m = projectile_load_random_rotation(c.rng);
                o.proj.as_mut().unwrap().mtx = m;
            }
            let (bouncecount, sticky, hsp, speedy, forcegood) = {
                let p = o.proj.as_mut().unwrap();
                p.bouncecount += 1;
                p.bounceframe = lvframe60;
                (p.bouncecount, p.flags & PROJECTILEFLAG_STICKY != 0, p.hitspeedpreservationfrac, p.speed.y, p.flags & PROJECTILEFLAG_FORCEGOODBOUNCE != 0)
            };
            if atground {
                if !sticky && bouncecount >= 6 {
                    if settle {
                        projectile_settle(o, &realrot, c.rng, lv);
                    }
                } else if hsp > 0.0 {
                    if speedy >= 0.0 && speedy < 2.222_222_3 {
                        if forcegood && bouncecount == 1 {
                            o.proj.as_mut().unwrap().speed.y = 2.222_222_3;
                        } else if settle {
                            projectile_settle(o, &realrot, c.rng, lv);
                        }
                    }
                } else if settle {
                    projectile_settle(o, &realrot, c.rng, lv);
                }
            }
        }

        if o.ty == ObjType::Weapon {
            weapon_flight_extras(o, c, collided, atground, prevpos);
        }
    }
    moved
}

/// The per-weapon tail of the AIRBORNE branch: knife woosh, rocket power and
/// trails, grenade-round detonation on landing, and the collision sounds.
fn weapon_flight_extras(o: &mut WorldObj, c: &mut ObjCtx, collided: bool, atground: bool, prevpos: Vec3) {
    let lv = c.lv;
    let Some(p) = o.proj.as_mut() else {
        return;
    };
    if o.weaponnum == WEAPON_COMBATKNIFE && o.gunfunc == FUNC_SECONDARY {
        // knife_play_woosh_sound (`propobj.c:3919`).
        if p.flags & PROJECTILEFLAG_AIRBORNE != 0 && p.bouncecount <= 0 && o.thrownknife {
            let _ = c.rng.random() % 3;
            if p.lastwooshframe < lv.lvframe60 - 6 {
                c.out.sounds.push((0x8074, o.pos, 1.0));
                p.lastwooshframe = lv.lvframe60;
            }
        } else {
            o.thrownknife = false;
        }
    } else if o.weaponnum == WEAPON_ROCKET {
        if collided {
            o.timer240 = 0;
        } else {
            if p.speed.length_squared() > 27_777.773 {
                p.accel = Vec3::ZERO;
            }
            if p.powerlimit240 >= 0 && p.flighttime240 > p.powerlimit240 {
                p.missileyaccel = 0.0;
                p.flags &= !(PROJECTILEFLAG_POWERED | PROJECTILEFLAG_MISSILE);
            } else {
                let d = p.speed.normalize_or_zero();
                c.smokes.smoke_create_simple(o.pos - d * 20.0, smoke::SMOKETYPE_ROCKETTAIL);
            }
        }
    } else if o.weaponnum == WEAPON_HOMINGROCKET {
        if collided {
            o.timer240 = 0;
        } else {
            c.smokes.smoke_create_simple(o.pos, smoke::SMOKETYPE_HOMINGTAIL);
        }
    } else if o.weaponnum == WEAPON_GRENADEROUND || (o.weaponnum == WEAPON_NBOMB && o.gunfunc == FUNC_PRIMARY) {
        let slow = p.speed.abs().max_element() < 0.1;
        let still = (o.pos - prevpos).abs().max_element() < 0.1;
        if atground || p.flags & PROJECTILEFLAG_SETTLING != 0 || slow || still {
            if o.weaponnum != WEAPON_NBOMB || o.timer240 >= 0 {
                o.timer240 = 0;
            }
        } else if o.weaponnum != WEAPON_NBOMB {
            c.smokes.smoke_create_simple(o.pos, smoke::SMOKETYPE_GRENADETAIL);
        }
    }
    if collided {
        if p.collisionframe < lv.lvframenum - 2 {
            if o.weaponnum == WEAPON_COMBATKNIFE {
                c.out.sounds.push((0x808b, o.pos, 1.0));
            } else if o.weaponnum == WEAPON_GRENADE && o.gunfunc == FUNC_SECONDARY {
                const SOUNDS: [u16; 4] = [0x0027, 0x0028, 0x0029, 0x002a];
                let s = SOUNDS[(c.rng.random() % 4) as usize];
                c.out.sounds.push((s, o.pos, 1.0));
                c.out.sounds.push((0x808c, o.pos, 1.0));
            } else {
                c.out.sounds.push((0x808c, o.pos, 1.0));
            }
        }
        p.collisionframe = lv.lvframenum;
    }
}

/// `obj_get_ground_clearance` (`propobj.c:2190`).
fn obj_get_ground_clearance(o: &WorldObj) -> f32 {
    if o.ty == ObjType::Weapon {
        0.0
    } else {
        4.0
    }
}

fn tick_settling(o: &mut WorldObj, c: &mut ObjCtx, sp5dc: &mut Vec3) -> bool {
    let lv = c.lv;
    let mut moved = false;
    let mut stop = true;
    {
        let p = o.proj.as_mut().unwrap();
        if p.settledrotfrac < 1.0 {
            p.settledrotfrac += p.settledrotinc * lv.lvupdate60freal;
            if lv.lvupdate60 > 0 {
                p.settledrotinc *= 1.1;
            }
            if p.settledrotfrac > 1.0 {
                p.settledrotfrac = 1.0;
            }
            let q = p.unk068.slerp(p.unk078, p.settledrotfrac);
            let m = Mat3::from_quat(q);
            o.realrot = Mat3::from_cols(m.x_axis * p.unk0b8[0], m.y_axis * p.unk0b8[1], m.z_axis * p.unk0b8[2]);
            stop = false;
        }
    }
    let (sx, sz, frac) = {
        let p = o.proj.as_ref().unwrap();
        (p.speed.x, p.speed.z, p.settledrotfrac)
    };
    if sx != 0.0 || sz != 0.0 || frac < 1.0 {
        let sp98 = o.bbox.rotated_y_min(&o.realrot);
        stop = false;
        {
            let p = o.proj.as_mut().unwrap();
            for _ in 0..lv.lvupdate60 {
                sp5dc.x += p.speed.x;
                sp5dc.z += p.speed.z;
                if p.settledrotfrac >= 1.0 {
                    if p.speeddecel > 0.0 {
                        let dist = (p.speed.x * p.speed.x + p.speed.z * p.speed.z).sqrt();
                        if dist > 0.0 {
                            let f12 = p.speeddecel * lv.lvupdate60freal / dist;
                            if f12 >= 1.0 {
                                p.speed.x = 0.0;
                                p.speed.z = 0.0;
                            } else {
                                p.speed.x -= p.speed.x * f12;
                                p.speed.z -= p.speed.z * f12;
                            }
                        } else {
                            p.speed.x = 0.0;
                            p.speed.z = 0.0;
                        }
                    } else {
                        p.speed.x *= 0.9;
                        p.speed.z *= 0.9;
                    }
                }
            }
        }
        let prevpos = o.pos;
        let mut n = Vec3::ZERO;
        func0f06d37c(o, c.world, Vec3::new(sp5dc.x, o.pos.y, sp5dc.z), &mut n);
        moved = true;
        match ground_under(c.world, o.pos.x, o.pos.z, prevpos.y) {
            Some((spa4, _)) => o.pos.y = spa4 - sp98 + obj_get_ground_clearance(o),
            None => {
                o.pos = prevpos;
                let p = o.proj.as_mut().unwrap();
                p.speed.x = 0.0;
                p.speed.z = 0.0;
            }
        }
        let p = o.proj.as_mut().unwrap();
        if p.speed.x.abs() < 0.1 && p.speed.z.abs() < 0.1 {
            p.speed.x = 0.0;
            p.speed.z = 0.0;
        }
    }
    if stop {
        // obj_free_projectile: it has come to rest.
        o.proj = None;
    }
    moved
}

/// `projectile_settle` (`propobj.c:3658`): pick the face the object comes to
/// rest on and start slerping to it. `arg1` is `realrot` before this tick's spin.
fn projectile_settle(o: &mut WorldObj, arg1: &Mat3, rng: &mut Rng, lv: Lv) {
    let bbox = o.bbox;
    let realrot = o.realrot;
    let Some(p) = o.proj.as_mut() else { return };
    p.has_owner = false;
    p.flags &= !PROJECTILEFLAG_AIRBORNE;
    p.flags |= PROJECTILEFLAG_SETTLING;
    p.flags &= !PROJECTILEFLAG_STICKY;

    let lens = [realrot.x_axis.length(), realrot.y_axis.length(), realrot.z_axis.length()];
    let sp108 = Mat3::from_cols(realrot.x_axis / lens[0], realrot.y_axis / lens[1], realrot.z_axis / lens[2]);
    p.unk068 = Quat::from_mat3(&sp108).normalize();
    p.unk0b8 = lens;

    let next = |i: usize| (i + 1) % 3;
    let prev = |i: usize| (i + 2) % 3;
    let col = |m: &Mat3, i: usize| match i {
        0 => m.x_axis,
        1 => m.y_axis,
        _ => m.z_axis,
    };
    let localsizes = [bbox.xmax - bbox.xmin, bbox.ymax - bbox.ymin, bbox.zmax - bbox.zmin];
    let mut unksizes = [0.0f32; 3];
    let mut rotatedsizes = [0.0f32; 3];
    for i in 0..3 {
        unksizes[i] = localsizes[i] * p.unk0b8[i];
        rotatedsizes[i] = (col(&realrot, i).y * localsizes[i]).abs();
    }
    let (mut lside, mut sside, mut mside): (i32, i32, i32) = (-1, -1, -1);
    if o.settlerot_byactualsize || o.settlerot_laptop {
        if o.settlerot_byactualsize {
            for i in 0..3 {
                if unksizes[i] < unksizes[next(i)] && unksizes[i] < unksizes[prev(i)] {
                    sside = i as i32;
                    break;
                }
            }
        } else {
            sside = 1;
        }
        if sside >= 0 {
            let s = sside as usize;
            if rotatedsizes[next(s)] >= rotatedsizes[prev(s)] {
                lside = next(s) as i32;
                mside = prev(s) as i32;
            } else {
                lside = prev(s) as i32;
                mside = next(s) as i32;
            }
        }
    }
    if lside < 0 {
        // One side three times the others (a gun): lie along it.
        for i in 0..3 {
            if unksizes[i] > unksizes[next(i)] * 3.0 && unksizes[i] > unksizes[prev(i)] * 3.0 {
                lside = i as i32;
                if unksizes[next(i)] > unksizes[prev(i)] * 2.0 {
                    sside = prev(i) as i32;
                    mside = next(i) as i32;
                } else if unksizes[prev(i)] > unksizes[next(i)] * 2.0 {
                    sside = next(i) as i32;
                    mside = prev(i) as i32;
                } else if rng.random() % 2 == 0 {
                    sside = prev(i) as i32;
                    mside = next(i) as i32;
                } else {
                    sside = next(i) as i32;
                    mside = prev(i) as i32;
                }
                break;
            }
        }
    }
    if lside < 0 {
        // Squarish: any side three times another.
        for i in 0..3 {
            if unksizes[i] > unksizes[next(i)] * 3.0 || unksizes[i] > unksizes[prev(i)] * 3.0 {
                let s = if unksizes[i] > unksizes[next(i)] * 3.0 { next(i) } else { prev(i) };
                sside = s as i32;
                if rotatedsizes[next(s)] >= rotatedsizes[prev(s)] {
                    lside = next(s) as i32;
                    mside = prev(s) as i32;
                } else {
                    lside = prev(s) as i32;
                    mside = next(s) as i32;
                }
                break;
            }
        }
    }
    if lside < 0 {
        // Cubish. PD's @bug (>= where <= was meant) is why grenades land upright.
        for i in 0..3 {
            if rotatedsizes[i] >= rotatedsizes[next(i)] && rotatedsizes[i] >= rotatedsizes[prev(i)] {
                sside = i as i32;
                if rotatedsizes[next(i)] >= rotatedsizes[prev(i)] {
                    mside = prev(i) as i32;
                    lside = next(i) as i32;
                } else {
                    lside = prev(i) as i32;
                    mside = next(i) as i32;
                }
                break;
            }
        }
    }
    if lside < 0 {
        lside = 0;
        sside = 1;
        mside = 2;
    }
    let (l, s, m) = (lside as usize, sside as usize, mside as usize);
    let mut xrot = col(&realrot, l).x;
    let mut zrot = col(&realrot, l).z;
    if xrot != 0.0 || zrot != 0.0 {
        let f0 = (xrot * xrot + zrot * zrot).sqrt();
        if f0 > 0.0 {
            xrot /= f0;
            zrot /= f0;
        } else {
            xrot = 0.0;
            zrot = 1.0;
        }
    } else {
        xrot = 0.0;
        zrot = 1.0;
    }
    let mut cols = [Vec3::ZERO; 3];
    cols[l] = Vec3::new(xrot, 0.0, zrot);
    let sy = col(&realrot, s).y;
    let laptop = o.settlerot_laptop;
    cols[m] = if ((sy >= 0.0 || laptop) && m == next(s)) || (sy <= 0.0 && !laptop && m == prev(s)) {
        Vec3::new(-zrot, 0.0, xrot)
    } else {
        Vec3::new(zrot, 0.0, -xrot)
    };
    cols[s] = if sy >= 0.0 || laptop { Vec3::Y } else { -Vec3::Y };
    let spc8 = Mat3::from_cols(cols[0], cols[1], cols[2]);
    p.unk078 = Quat::from_mat3(&spc8).normalize();
    // quaternion0f0976c0: take the short way round.
    if p.unk068.dot(p.unk078) < 0.0 {
        p.unk078 = -p.unk078;
    }
    p.settledrotfrac = 0.0;
    let sp6c = cols[l].dot(col(&sp108, l)).clamp(-1.0, 1.0).acos();
    let ry = col(&realrot, l).y;
    let ay = col(arg1, l).y;
    p.settledrotinc = if sp6c > 0.0 && ry > 0.0 && ry > ay {
        0.05 / (sp6c * 0.636_721_13)
    } else if sp6c > 0.0 && ry < 0.0 && ry < ay {
        0.05 / (sp6c * 0.636_721_13)
    } else {
        let sc = o.scale * o.scale;
        let f2 = (col(arg1, l).dot(col(&realrot, l)) / sc).clamp(-1.0, 1.0).acos() / lv.lvupdate60freal.max(0.25);
        if sp6c != 0.0 {
            f2 / sp6c
        } else {
            1.0
        }
    };
    p.settledrotinc = p.settledrotinc.abs().clamp(0.03, 0.15);
}

/// `obj_stick` (`propobj.c:4150`) for the background or a target board.
fn obj_stick(o: &mut WorldObj, c: &mut ObjCtx, pos: Vec3, rot: Vec3, board: Option<usize>) {
    o.proj = None;
    o.attached = true;
    match o.ty {
        ObjType::Weapon => match o.weaponnum {
            WEAPON_BOLT => obj_stick_bolt(o, c, pos),
            WEAPON_COMBATKNIFE => obj_stick_knife(o, c, pos, rot),
            _ => obj_stick_default(o, pos, rot),
        },
        ObjType::Autogun => {
            obj_stick_default(o, pos, rot);
            if let Some(a) = o.autogun.as_mut() {
                a.yzero = pd_atan2f(rot.x, rot.z);
                a.xzero = pd_atan2f(rot.y, (rot.x * rot.x + rot.z * rot.z).sqrt());
                a.xrot = a.xzero;
                a.yrot = a.yzero;
            }
        }
    }
    match board {
        Some(b) => {
            // bgun_play_prop_hit_sound + obj_embed into the (static) board.
            let id = if c.rng.random() % 2 == 0 { 0x8089 } else { 0x808a };
            c.out.sounds.push((id, pos, 1.0));
            o.embedded_board = Some(b);
        }
        None => {
            if o.ty == ObjType::Weapon {
                c.out.bg_hit_sounds.push((o.weaponnum, pos));
            }
        }
    }
}

/// `obj_stick_default` (`propobj.c:4006`): stand local +y on the normal, the
/// bbox bottom on the surface.
fn obj_stick_default(o: &mut WorldObj, pos: Vec3, rot: Vec3) {
    let mut sp40 = func0f06e9cc(rot);
    pdmtx::scale3(&mut sp40, o.scale);
    let ymin = o.bbox.ymin;
    let y = sp40.y_axis.truncate();
    o.pos = pos - y * ymin;
    o.realrot = Mat3::from_mat4(sp40);
}

/// `obj_stick_bolt` (`propobj.c:4026`): keep the flight orientation, sink the
/// tip in, and start the quiver (`timer240 = 13`).
fn obj_stick_bolt(o: &mut WorldObj, c: &mut ObjCtx, pos: Vec3) {
    o.timer240 = 13;
    let zmax = o.bbox.zmax - (25.0 + 2.0 * c.rng.randomfrac());
    o.pos = pos - o.realrot.z_axis * zmax;
}

/// `obj_stick_knife` (`propobj.c:4061`).
fn obj_stick_knife(o: &mut WorldObj, c: &mut ObjCtx, pos: Vec3, rot: Vec3) {
    let _ = o.bbox.zmin - (25.0 + 2.0 * c.rng.randomfrac());
    let sp1c = Vec3::new(
        c.rng.randomfrac() * 0.8 + rot.x - 0.4,
        c.rng.randomfrac() * 0.8 + rot.y - 0.4,
        c.rng.randomfrac() * 0.8 + rot.z - 0.4,
    );
    let sp90 = func0f06e9cc(sp1c);
    let sp50 = pdmtx::load_x_rotation(baddtor(-90.0));
    let mut spd0 = pdmtx::mul(&sp90, &sp50);
    pdmtx::scale3(&mut spd0, o.scale);
    o.pos = pos;
    o.realrot = Mat3::from_mat4(spd0);
}

/// `prop_explode` (`propobj.c:4223`): stuck to the background → a scorch on
/// that surface; otherwise `explosion_create_complex`.
pub fn prop_explode(o: &WorldObj, c: &mut ObjCtx, exptype: usize) -> bool {
    let world: &dyn explosions::ExpWorld = c.world;
    if o.attached && o.proj.is_none() && o.embedded_board.is_none() {
        let ymin = o.bbox.ymin;
        let n = o.realrot.y_axis;
        let scorch = o.pos + n * ymin;
        c.explosions.create(c.rng, c.smokes, world, None, o.pos, exptype, o.owner, true, scorch, n, c.campos, c.lodscalez)
    } else {
        c.explosions.create_complex(c.rng, c.smokes, world, None, o.pos, exptype, o.owner, c.campos, c.lodscalez)
    }
}

/// `weapon_tick` (`propobj.c:4308`): fuses, the wall hugger, N-Bombs, rockets,
/// timed/remote/proximity mines, bolts.
pub fn weapon_tick(o: &mut WorldObj, c: &mut ObjCtx) {
    let lv240 = c.lv.lvupdate240;
    let w = o.weaponnum;
    if ((w == WEAPON_GRENADE && o.gunfunc == FUNC_PRIMARY) || w == WEAPON_GRENADEROUND) && o.timer240 >= 0 {
        if w == WEAPON_GRENADEROUND && o.gunfunc == FUNC_SECONDARY && o.timer240 > 0 {
            if o.timer240 >= 2 {
                // Still on the wall.
                o.timer240 -= lv240;
                if o.timer240 < 8 {
                    // Time to fall.
                    let mut p = o.proj.take().unwrap_or_default();
                    p.has_owner = false;
                    p.flags |= PROJECTILEFLAG_AIRBORNE | PROJECTILEFLAG_STICKY;
                    p.speed = Vec3::new(0.0, -10.0, 0.0);
                    p.mtx = Mat3::IDENTITY;
                    p.startframe = c.lv.lvframenum;
                    o.proj = Some(p);
                    o.attached = false;
                    o.timer240 = 1;
                }
            }
        } else {
            o.timer240 -= lv240;
            if o.timer240 < 0 {
                o.dangerous = false;
                let t = if o.gunfunc == FUNC_2 { explosions::EXPLOSIONTYPE_SDGRENADE } else { explosions::EXPLOSIONTYPE_ROCKET };
                prop_explode(o, c, t);
                o.deleting = true;
            }
        }
    } else if w == WEAPON_NBOMB && o.gunfunc == FUNC_PRIMARY {
        // Impact N-Bombs go off on landing (projectile_tick); this is the
        // airborne-the-whole-time fallback.
        if o.timer240 >= 0 {
            o.timer240 -= lv240;
            if o.timer240 < 0 {
                c.out.nbombs.push(o.pos);
                o.dangerous = false;
                o.deleting = true;
            }
        }
    } else if w == WEAPON_ROCKET || w == WEAPON_HOMINGROCKET {
        if o.timer240 == 0 {
            prop_explode(o, c, explosions::EXPLOSIONTYPE_ROCKET);
            o.deleting = true;
        }
    } else if w == WEAPON_TIMEDMINE && o.timer240 >= 0 {
        if o.gunfunc == FUNC_PRIMARY {
            o.timer240 -= lv240;
            if o.timer240 < 0 && prop_explode(o, c, explosions::EXPLOSIONTYPE_ROCKET) {
                o.timer240 = -1;
                o.deleting = true;
            }
        }
    } else if w == WEAPON_REMOTEMINE {
        if c.detonating {
            o.timer240 = 0;
        }
        if o.timer240 >= 2 {
            o.timer240 -= lv240;
            if o.timer240 < 2 {
                o.timer240 = 1;
            }
        } else if o.timer240 == 0 && prop_explode(o, c, explosions::EXPLOSIONTYPE_ROCKET) {
            o.timer240 = -1;
            o.deleting = true;
        }
    } else if w == WEAPON_PROXIMITYMINE
        || (w == WEAPON_DRAGON && o.gunfunc == FUNC_SECONDARY)
        || (w == WEAPON_GRENADE && o.gunfunc == FUNC_SECONDARY)
        || (w == WEAPON_NBOMB && o.gunfunc == FUNC_SECONDARY)
    {
        if o.timer240 >= 2 {
            // Arming.
            o.timer240 -= lv240;
            if o.timer240 < 2 {
                o.timer240 = 1;
            }
        } else if o.timer240 == 1 {
            // Armed: the player within 2.5 m sets it off (their own included).
            if (c.playerpos - o.pos).length_squared() < 250.0 * 250.0 {
                o.timer240 = 0;
            }
        }
        if o.timer240 == 0 {
            if w == WEAPON_NBOMB {
                c.out.nbombs.push(o.pos);
                o.dangerous = false;
                o.deleting = true;
            } else {
                let t = if w == WEAPON_DRAGON { explosions::EXPLOSIONTYPE_DRAGONBOMBSPY } else { explosions::EXPLOSIONTYPE_ROCKET };
                if prop_explode(o, c, t) {
                    o.timer240 = -1;
                    o.deleting = true;
                }
            }
        }
    } else if w == WEAPON_BOLT {
        // The quiver once stuck: timer240 13 → 2 rocks it about its tip.
        if o.timer240 >= 2 {
            let ival = o.timer240 - 1;
            let mut radians = 0.026_179_94 * (ival as f32 / 12.0);
            if ival < 12 {
                radians += 0.026_179_94 * ((ival + 1) as f32 / 12.0);
            }
            if ival & 1 == 1 {
                radians = -radians;
            }
            let spb8 = Mat3::from_mat4(pdmtx::load_y_rotation(radians));
            let zmax = o.bbox.zmax;
            let sp6c = o.realrot * Vec3::new(0.0, 0.0, zmax);
            let sp78 = o.realrot * spb8;
            let sp60 = sp78 * Vec3::new(0.0, 0.0, zmax);
            o.realrot = sp78;
            o.pos -= sp60 - sp6c;
            o.timer240 -= 1;
        }
        // (The bolt's beam trail — boltbeams — isn't drawn.)
    }
}
