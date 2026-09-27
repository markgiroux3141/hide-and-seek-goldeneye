//! `autogunobj` — the Laptop Gun deployed as a sentry, ported from `propobj.c`
//! (`laptop_deploy`, `autogun_tick`, `autogun_init_matrices`,
//! `autogun_tick_shoot`, `apply_speed`, `apply_rotation`) and `training.c`
//! (`fr_choose_autogun_target`, `fr_is_target_facing_pos`) — PD's own
//! firing-range behaviour, where a deployed laptop picks off the targets.
//!
//! Substituted: the LOS and shot traces are the range's raycast; the sentry
//! itself can't be shot (the range's raycast doesn't know about objects).

use glam::{Mat4, Vec3};

use super::bgun::Lv;
use super::fx::{self, Beam, FxBatch, FxKind, FxVert};
use super::gset::*;
use super::model::{ModelDef, NodeKind};
use super::pdmtx;
use super::props::{pd_atan2f, WorldObj};
use super::range::{HitKind, Range};
use crate::pd_spike::pdmath::{baddtor, dtor, Rng};

pub const MODELPART_AUTOGUN_0001: i32 = 0x01;
pub const MODELPART_AUTOGUN_0002: i32 = 0x02;
pub const MODELPART_AUTOGUN_0003: i32 = 0x03;
pub const MODELPART_AUTOGUN_FLASHLEFT: i32 = 0x05;
pub const MODELPART_AUTOGUN_FLASHRIGHT: i32 = 0x07;

/// `struct autogunobj` beyond `defaultobj` (+ the obj flags it uses).
#[derive(Clone, Debug, Default)]
pub struct Autogun {
    pub yzero: f32,
    pub xzero: f32,
    pub yrot: f32,
    pub xrot: f32,
    pub yspeed: f32,
    pub xspeed: f32,
    pub barrelspeed: f32,
    pub barrelrot: f32,
    pub ymaxleft: f32,
    pub ymaxright: f32,
    pub maxspeed: f32,
    pub aimdist: f32,
    pub firecount: i32,
    pub firing: bool,
    pub ammoquantity: i32,
    pub lastseebond60: i32,
    pub lastaimbond60: i32,
    pub allowsoundframe: i32,
    pub shotbondsum: f32,
    /// The current target (`autogun->target`): an index into the range's targets.
    pub target: Option<usize>,
    /// When the target is a chr, which one (the range's target list is rebuilt
    /// every frame, so the index is re-found from this).
    pub target_chr: Option<usize>,
    /// `autogun->nextchrtest`: the round-robin over the chrs (starts at -1).
    pub nextchrtest: i32,
    /// `OBJFLAG_AUTOGUN_SEENTARGET`.
    pub seentarget: bool,
    /// The flash toggles this tick (`chrgunfire.visible`).
    pub fireleft: bool,
    pub fireright: bool,
    /// `g_ThrownLaptopBeams[i]`.
    pub beam: Beam,
}

/// What a sentry's shots did this tick.
#[derive(Default, Debug)]
pub struct AutogunOut {
    pub sounds: Vec<(u16, Vec3)>,
    pub bg_hit_sounds: Vec<(i32, Vec3)>,
    pub sparks: Vec<(Vec3, usize)>,
    pub board_hits: Vec<(usize, Vec3)>,
    /// Chrs a round hit: (chr, damage, position, direction).
    pub chr_hits: Vec<(usize, f32, Vec3, Vec3)>,
}

/// `apply_speed` (`propobj.c:3562`): move `distdone` towards `maxdist` with
/// accel/decel and a speed cap, stopping exactly on target.
pub fn apply_speed(lv: Lv, distdone: &mut f32, maxdist: f32, speedptr: &mut f32, accel: f32, decel: f32, maxspeed: f32) {
    let mut speed = *speedptr;
    for _ in 0..lv.lvupdate60 {
        let limit = speed * speed * 0.5 / decel;
        let distremaining = maxdist - *distdone;
        if distremaining > 0.0 {
            if speed > 0.0 && distremaining <= limit {
                speed -= decel;
                if speed < decel {
                    speed = decel;
                }
            } else if speed < maxspeed {
                if speed < 0.0 {
                    speed += decel;
                } else {
                    speed += accel;
                }
                if speed > maxspeed {
                    speed = maxspeed;
                }
            }
            if speed >= distremaining {
                *distdone = maxdist;
                break;
            }
            *distdone += speed;
        } else {
            if speed < 0.0 && -distremaining <= limit {
                speed += decel;
                if speed > -decel {
                    speed = -decel;
                }
            } else if speed > -maxspeed {
                if speed > 0.0 {
                    speed -= decel;
                } else {
                    speed -= accel;
                }
                if speed < -maxspeed {
                    speed = -maxspeed;
                }
            }
            if speed <= distremaining {
                *distdone = maxdist;
                break;
            }
            *distdone += speed;
        }
    }
    *speedptr = speed;
}

/// `apply_rotation` (`propobj.c:3629`): `apply_speed` on an angle, the short
/// way round, kept in 0..2π.
pub fn apply_rotation(lv: Lv, angle: &mut f32, maxrot: f32, speed: &mut f32, accel: f32, decel: f32, maxspeed: f32) {
    let mut maxrot = maxrot;
    let tmp = maxrot - *angle;
    if tmp < dtor(-180.0) {
        maxrot += baddtor(360.0);
    } else if tmp >= dtor(180.0) {
        maxrot -= baddtor(360.0);
    }
    apply_speed(lv, angle, maxrot, speed, accel, decel, maxspeed);
    if *angle < 0.0 {
        *angle += baddtor(360.0);
    }
    if *angle >= baddtor(360.0) {
        *angle -= baddtor(360.0);
    }
}

/// `chr_get_aim_limit_angle` (`chraction.c:9450`).
pub fn chr_get_aim_limit_angle(sqdist: f32) -> f32 {
    if sqdist > 1600.0 * 1600.0 {
        baddtor(1.074_626_8)
    } else if sqdist > 800.0 * 800.0 {
        baddtor(2.155_688_5)
    } else if sqdist > 400.0 * 400.0 {
        baddtor(4.285_714)
    } else if sqdist > 200.0 * 200.0 {
        baddtor(8.571_428)
    } else {
        baddtor(14.4)
    }
}

/// `fr_get_target_angle_to_pos` (`training.c:1455`). The range's boards all face
/// down-range towards -z, which is `targetangle` 0.
fn fr_get_target_angle_to_pos(targetpos: Vec3, targetangle: f32, pos: Vec3) -> f32 {
    let directangle = pd_atan2f(targetpos.x - pos.x, targetpos.z - pos.z);
    let mut rel = directangle - targetangle;
    if directangle < targetangle {
        rel += baddtor(360.0);
    }
    rel
}

fn facing(targetpos: Vec3, pos: Vec3) -> bool {
    let a = fr_get_target_angle_to_pos(targetpos, 0.0, pos);
    !(a > dtor(90.0) && a < baddtor(270.0))
}

fn board_pos(range: &Range, i: usize) -> Vec3 {
    let t = &range.targets[i];
    (t.bbox.min + t.bbox.max) * 0.5
}

/// `fr_choose_autogun_target` (`training.c:1494`): the closest board facing
/// the laptop.
fn fr_choose_autogun_target(range: &Range, gunpos: Vec3) -> Option<usize> {
    let mut best = None;
    let mut closest = f32::MAX;
    for i in 0..range.targets.len() {
        let p = board_pos(range, i);
        if facing(p, gunpos) {
            let d = (p - gunpos).length_squared();
            if d < closest {
                closest = d;
                best = Some(i);
            }
        }
    }
    best
}

impl Autogun {
    /// `autogun_tick` (`propobj.c:8557`), the regular (not malfunctioning /
    /// windmill) behaviour, in the firing range.
    pub fn tick(&mut self, gunpos: Vec3, range: &Range, lv: Lv) {
        let mut target = None;
        let mut awake = false;
        let mut spinup = false;
        let mut insight = false;
        let mut limitangle = 0.0;
        // A chr target is found again by chr (the target list is rebuilt per frame).
        if let Some(c) = self.target_chr {
            self.target = range.targets.iter().position(|t| t.chr == Some(c));
            if self.target.is_none() {
                self.target_chr = None;
            }
        }
        let chrs: Vec<usize> = (0..range.targets.len()).filter(|&i| range.targets[i].chr.is_some()).collect();
        if self.ammoquantity == 0 {
            // No target.
        } else if self.target.is_some() {
            target = self.target;
        } else if !chrs.is_empty() {
            // Multiplayer (`propobj.c:8676`): one chr tried per tick, round-robin
            // over `g_MpAllChrPtrs`; the owner, the dead and the hidden aren't in
            // the host's target list.
            self.nextchrtest += 1;
            if self.nextchrtest >= chrs.len() as i32 {
                self.nextchrtest = -1;
            } else {
                target = Some(chrs[self.nextchrtest as usize]);
            }
        } else {
            target = fr_choose_autogun_target(range, gunpos);
        }
        let mut goalyrot = self.yzero;
        let mut goalxrot = self.xzero;
        if let Some(t) = target {
            let is_chr = range.targets[t].chr.is_some();
            let tp = board_pos(range, t);
            let d = tp - gunpos;
            let sqdist = d.x * d.x + d.z * d.z;
            let dist = sqdist.sqrt();
            let horizdist = dist;
            limitangle = chr_get_aim_limit_angle(sqdist);
            if dist <= self.aimdist {
                let targetangleh = pd_atan2f(d.x, d.z);
                let targetanglev = pd_atan2f(d.y, horizdist);
                if self.seentarget {
                    awake = true;
                } else {
                    let mut f12 = targetangleh - self.yrot;
                    if f12 < 0.0 {
                        f12 += baddtor(360.0);
                    }
                    if f12 > baddtor(180.0) {
                        f12 -= baddtor(360.0);
                    }
                    if f12 < baddtor(70.0) && f12 > baddtor(-70.0) {
                        awake = true;
                    }
                }
                if awake {
                    let mut relangleh = targetangleh - self.yzero;
                    if relangleh < dtor(-180.0) {
                        relangleh += baddtor(360.0);
                    } else if relangleh >= dtor(180.0) {
                        relangleh -= baddtor(360.0);
                    }
                    // A board is trackable while it faces the gun; a live chr
                    // always (`propobj.c:8837`).
                    let track = is_chr || facing(tp, gunpos);
                    // cd_test_los_oobfail(…, GEOFLAG_BLOCK_SIGHT) with both
                    // perimeters off: clear to the target.
                    let los = match (&range.tiles, is_chr) {
                        (Some(tiles), true) => tiles.los(gunpos, tp),
                        _ => match range.raycast(gunpos, d.normalize_or_zero(), d.length()) {
                            None => true,
                            Some(h) => h.kind == HitKind::Target(t),
                        },
                    };
                    if relangleh <= self.ymaxleft && relangleh >= self.ymaxright && track && los {
                        self.seentarget = true;
                        insight = true;
                        goalxrot = targetanglev;
                        goalyrot = targetangleh;
                        if self.target.is_none() {
                            self.target = Some(t);
                            self.target_chr = range.targets[t].chr;
                        }
                    } else if self.lastseebond60 >= 0 && self.lastseebond60 > lv.lvframe60 - 120 {
                        goalyrot = self.yrot;
                        goalxrot = self.xrot;
                    } else {
                        awake = false;
                    }
                }
            }
        }
        if !awake {
            self.target = None;
            self.target_chr = None;
        }
        // The turret swivels left and right while firing.
        if self.firing {
            goalyrot += limitangle * 0.8 * ((lv.lvframe60 % 120) as f32 * baddtor(3.0)).sin();
            if goalyrot < 0.0 {
                goalyrot += baddtor(360.0);
            }
            if goalyrot >= baddtor(360.0) {
                goalyrot -= baddtor(360.0);
            }
        }
        let mut f0 = goalyrot - self.yzero;
        if f0 < dtor(-180.0) {
            f0 += baddtor(360.0);
        } else if f0 >= dtor(180.0) {
            f0 -= baddtor(360.0);
        }
        if f0 > self.ymaxleft {
            goalyrot = self.yzero + self.ymaxleft;
        } else if f0 < self.ymaxright {
            goalyrot = self.yzero + self.ymaxright;
        }
        if goalyrot < 0.0 {
            goalyrot += baddtor(360.0);
        }
        if goalyrot >= baddtor(360.0) {
            goalyrot -= baddtor(360.0);
        }
        let acc = 0.000_872_525_7;
        apply_rotation(lv, &mut self.yrot, goalyrot, &mut self.yspeed, acc, acc, self.maxspeed);
        apply_rotation(lv, &mut self.xrot, goalxrot, &mut self.xspeed, acc, acc, self.maxspeed);
        let wrap = |mut a: f32| {
            if a < 0.0 {
                a += baddtor(360.0);
            }
            if a > baddtor(180.0) {
                a -= baddtor(360.0);
            }
            a
        };
        let f12 = wrap(goalyrot - self.yrot);
        let f2 = wrap(goalxrot - self.xrot);
        self.firing = false;
        if awake {
            if f12 < limitangle && -limitangle < f12 && f2 < limitangle && -limitangle < f2 {
                self.firing = true;
                spinup = true;
                if insight {
                    self.lastseebond60 = lv.lvframe60;
                    self.lastaimbond60 = lv.lvframe60;
                }
            } else {
                let f0 = 2.0 * limitangle;
                if f12 < f0 && -f0 < f12 && f2 < f0 && -f0 < f2 {
                    self.firing = true;
                    spinup = true;
                    if insight {
                        self.lastseebond60 = lv.lvframe60;
                    }
                } else if self.lastseebond60 >= 0 && self.lastseebond60 > lv.lvframe60 - 120 {
                    self.firing = true;
                    spinup = true;
                }
            }
        }
        if spinup {
            self.barrelspeed = (self.barrelspeed + 0.009_971_722 * lv.lvupdate60freal).min(0.598_303_3);
        } else if self.barrelspeed > 0.0 {
            for _ in 0..lv.lvupdate60 {
                self.barrelspeed *= 0.99;
            }
            if self.barrelspeed <= 0.0001 {
                self.barrelspeed = 0.0;
            }
        }
        if self.barrelspeed > 0.0 {
            self.barrelrot += self.barrelspeed * lv.lvupdate60freal;
            while self.barrelrot >= baddtor(360.0) {
                self.barrelrot -= baddtor(360.0);
            }
        }
    }

    /// `autogun_init_matrices` (`propobj.c:9011`) in world space. Joint 1
    /// (the turret) yaws in WORLD terms — only its position follows the base.
    pub fn init_matrices(&self, def: &ModelDef, mats: &mut [Mat4]) {
        let root = mats[0];
        let part_pos = |part: i32| -> Option<(Vec3, usize)> {
            let node = def.get_part(part)?;
            match def.nodes[node].kind {
                NodeKind::Position { pos, mtx, .. } => Some((pos, mtx[0] as usize)),
                _ => None,
            }
        };
        let mut yrot = self.yrot + baddtor(90.0);
        if yrot >= baddtor(360.0) {
            yrot -= baddtor(360.0);
        }
        let xrot = -self.xrot;
        let scale = root.x_axis.truncate().length();
        let Some((p1, i1)) = part_pos(MODELPART_AUTOGUN_0001) else { return };
        let sp4c = root.transform_point3(p1);
        let mut m1 = pdmtx::load_y_rotation(yrot);
        pdmtx::set_translation(&mut m1, sp4c);
        pdmtx::scale3(&mut m1, scale);
        if let Some(m) = mats.get_mut(i1) {
            *m = m1;
        }
        let Some((p2, i2)) = part_pos(MODELPART_AUTOGUN_0002) else { return };
        let mut m2 = pdmtx::load_z_rotation(xrot);
        pdmtx::set_translation(&mut m2, p2);
        let m2 = pdmtx::mul(&m1, &m2);
        if let Some(m) = mats.get_mut(i2) {
            *m = m2;
        }
        if let Some((p3, i3)) = part_pos(MODELPART_AUTOGUN_0003) {
            let mut m3 = pdmtx::load_x_rotation(self.barrelrot);
            pdmtx::set_translation(&mut m3, p3);
            if let Some(m) = mats.get_mut(i3) {
                *m = pdmtx::mul(&m2, &m3);
            }
        }
    }
}

/// The CHRGUNFIRE flash node of a model: (joint index, pos, dim, texture, size).
pub fn gunfire_node(def: &ModelDef, part: i32) -> Option<(usize, Vec3, Vec3, u16, [f32; 2])> {
    let node = def.get_part(part)?;
    let f = def.file.as_ref()?;
    let raw = f.nodes.get(node)?;
    let pos = Vec3::from(raw.pos?);
    let dim = Vec3::from(raw.dim?);
    let tex = raw.texture? as u16;
    let size = raw.texture_size.map_or([32.0, 32.0], |s| [s[0], s[1]]);
    let parent = def.nodes[node].parent?;
    Some((def.find_node_mtx_index(parent)?, pos, dim, tex, size))
}

/// `autogun_tick_shoot` (`propobj.c:9086`), the firing-range branch: every
/// other tick a round from the flash node along the turret's aim; a board it
/// hits scores (sparks + prop hit sound), anything else ricochets; every 4th
/// round draws a beam. SFXMAP_8044 every 4 ticks.
pub fn autogun_tick_shoot(o: &mut WorldObj, range: &Range, lv: Lv, rng: &mut Rng, out: &mut AutogunOut) {
    let mats = o.init_matrices();
    let def = o.def.clone();
    let objpos = o.pos;
    let Some(a) = o.autogun.as_mut() else { return };
    a.fireleft = false;
    a.fireright = false;
    if !a.firing {
        return;
    }
    a.firecount += 1;
    a.fireleft = a.firecount % 2 == 0;
    if def.get_part(MODELPART_AUTOGUN_FLASHRIGHT).is_some() {
        a.fireright = a.firecount % 2 == 1;
    }
    if a.fireleft || a.fireright {
        let makebeam = a.firecount % 4 == 0;
        // The shot starts at the flash (chrgunfire pos in its joint's frame),
        // or at the prop if a wall is in between.
        let mut gunpos = objpos;
        if let Some((mi, pos, _, _, _)) = gunfire_node(&def, MODELPART_AUTOGUN_FLASHLEFT) {
            gunpos = mats[mi].transform_point3(pos);
            let d = gunpos - objpos;
            if d.length() > 0.0 && range.raycast(objpos, d.normalize(), d.length()).is_some() {
                gunpos = objpos;
            }
        }
        let dir = Vec3::new(a.xrot.cos() * a.yrot.sin(), a.xrot.sin(), a.xrot.cos() * a.yrot.cos());
        let mut hitpos = gunpos + dir * 65536.0;
        let mut missed = false;
        let mut hitboard = None;
        if a.target.is_some() {
            if let Some(h) = range.raycast(gunpos, dir, 65536.0) {
                hitpos = h.pos;
                missed = true;
                if let HitKind::Target(b) = h.kind {
                    missed = false;
                    hitboard = Some(b);
                }
            }
        }
        if let Some(chr) = hitboard.and_then(|b| range.targets[b].chr) {
            // Multiplayer (`propobj.c:9186`): gset { WEAPON_RCP45 } at half
            // damage (1.8 × 0.5), HITPART_GENERAL, the prop hit sound and blood.
            out.chr_hits.push((chr, 1.8 * 0.5, hitpos, dir));
            out.sounds.push((0x8076, hitpos));
        } else if let Some(b) = hitboard {
            // fr_calculate_hit + sparks + bgun_play_prop_hit_sound.
            out.board_hits.push((b, hitpos));
            out.sparks.push((hitpos, fx::SPARKTYPE_DEFAULT));
            let id = if rng.random() % 2 == 0 { 0x8089 } else { 0x808a };
            out.sounds.push((id, hitpos));
        }
        if a.ammoquantity > 0 && a.ammoquantity != 255 {
            a.ammoquantity -= 1;
        }
        if missed {
            out.sparks.push((hitpos, fx::SPARKTYPE_DEFAULT));
            out.bg_hit_sounds.push((WEAPON_FALCON2, hitpos));
        }
        if makebeam {
            // PD's beam weapon is WEAPON_RCP45: an orange tracer.
            a.beam.create(rng, WEAPON_RCP120, gunpos, hitpos);
        }
    }
    if a.allowsoundframe < lv.lvframe60 {
        // MODEL_CHRAUTOGUN: SFXMAP_8044, one every 4 ticks.
        out.sounds.push((0x8044, objpos));
        a.allowsoundframe = 4 + lv.lvframe60;
    }
}

/// `model_render_node_chr_gunfire` (`model.c:3368`): the flash billboard,
/// facing the camera, 0.75–1.25 × `dim`, its texture square spun at random.
/// `seed` stands in for PD's render-time `random()` calls.
pub fn gunfire_geometry(def: &ModelDef, mats: &[Mat4], part: i32, campos: Vec3, seed: u32) -> Option<FxBatch> {
    let (mi, pos, dim, tex, size) = gunfire_node(def, part)?;
    let w = mats.get(mi)?;
    let p = w.transform_point3(pos);
    let scale_m = w.x_axis.truncate().length();
    let mut e = campos - p;
    let distance = e.length();
    e = if distance > 0.0 { e / (scale_m * distance) } else { Vec3::new(0.0, 0.0, 1.0 / scale_m) };
    let col = |i: usize| -> Vec3 {
        match i {
            0 => w.x_axis.truncate(),
            1 => w.y_axis.truncate(),
            _ => w.z_axis.truncate(),
        }
    };
    let spec = e.dot(col(1)).clamp(-1.0, 1.0).acos();
    let mut spf0 = (-(e.dot(col(2))) / spec.sin()).clamp(-1.0, 1.0).acos();
    if -(e.dot(col(0))) < 0.0 {
        spf0 = baddtor(360.0) - spf0;
    }
    let (spdc, spd8) = (spf0.cos(), spf0.sin());
    let (rot2, spd0) = (spec.cos(), spec.sin());
    let h = |k: u32| {
        let x = seed.wrapping_mul(0x9e37_79b9).wrapping_add(k.wrapping_mul(0x85eb_ca6b));
        (x ^ (x >> 15)).wrapping_mul(0x2c1b_3c6d) ^ (x >> 13)
    };
    let scale = 0.75 + (h(1) % 128) as f32 / 256.0;
    let d = dim * scale;
    let spcc = d.x * spdc * 0.5;
    let spc8 = d.z * spd8 * 0.5;
    let spc4 = d.y * spd0 * 0.5;
    let spc0 = d.x * rot2 * spd8 * 0.5;
    let spbc = d.z * rot2 * spdc * 0.5;
    let sp90 = Vec3::new(pos.x - d.x * 0.5, pos.y, pos.z);
    let v = [
        Vec3::new(sp90.x - spcc - spc0, sp90.y - spc4, sp90.z + spc8 - spbc),
        Vec3::new(sp90.x - spcc + spc0, sp90.y + spc4, sp90.z + spc8 + spbc),
        Vec3::new(sp90.x + spcc + spc0, sp90.y + spc4, sp90.z - spc8 + spbc),
        Vec3::new(sp90.x + spcc - spc0, sp90.y - spc4, sp90.z - spc8 - spbc),
    ];
    // Texture square rotated by a random angle (coss/sins · width · 0xb5 >> 18).
    let ang = ((h(2) as u16) as f32) / 65536.0 * std::f32::consts::TAU;
    // (±32767 · width · 181) >> 18 in 10.5 fixed point = 0.707 · width texels:
    // the texture's half-diagonal, so the whole square spins inside the quad.
    let r = size[0] * 181.0 / 256.0;
    let (c, s) = (ang.cos() * r, ang.sin() * r);
    let centre = size[0] * 0.5;
    let st = [[centre - c, centre - s], [centre + s, centre - c], [centre + c, centre + s], [centre - s, centre + c]];
    let colr = [1.0, 1.0, 1.0, 1.0];
    let fv = |i: usize| FxVert { pos: w.transform_point3(v[i]), st: st[i], col: colr };
    // gSPTri2(0, 1, 2, 2, 3, 0)
    Some(FxBatch { kind: FxKind::GunFire(tex), verts: vec![fv(0), fv(1), fv(2), fv(2), fv(3), fv(0)] })
}

impl super::sim::Sim {
    /// The autogun fields `laptop_deploy` (`propobj.c:17488`) starts with.
    pub fn laptop_deploy_state(&mut self) -> Autogun {
        // min(held Laptop ammo, 200) comes out of the reserve (unlimited here).
        Autogun {
            nextchrtest: -1,
            aimdist: 5000.0,
            ymaxleft: 12.56,
            ymaxright: -12.56,
            maxspeed: 0.0697,
            ammoquantity: 200,
            lastseebond60: -1,
            lastaimbond60: -1,
            allowsoundframe: -1,
            ..Autogun::default()
        }
    }
}
