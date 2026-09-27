//! The first-person controller: `game/bondmove.c` (input → look speeds, aim mode,
//! crosshair swivel), `game/bondwalk.c` (walking, crouch, lean) and
//! `game/bondhead.c` (the head-bob model), ported for one player in walk mode.
//!
//! PD's walk is animation-driven: `g_PlayerModeldef` plays the run/walk clips
//! (`g_HeadAnims`, `bondhead.c:14`) at a speed set by how hard you push, and the
//! **damped root motion of those clips is both the head bob and the distance you
//! travel** (`bwalk_update_horizontal`, `bondwalk.c:1611`). That is where PD's
//! ease-in and ease-out come from — there is no acceleration constant.
//!
//! Input mapping follows the PC port's `CONTROLMODE_PC` with `MOUSEAIM_CLASSIC`
//! (`pd-pcport/src/game/bondmove.c:1243`, defaults from `mplayer.c:129`), except
//! the gun-function toggle, which keeps the N64's hold-B behaviour.

use std::sync::Arc;

use glam::{Mat4, Vec3};

use super::anim::{update_chr_info, Anim, AnimCtx};
use super::animdata::AnimBank;
use super::bgun::*;
use super::gset::*;
use super::model::{player_head_modeldef, Model};
use super::pdmtx;
use crate::pd_spike::chraction::bmove_dampen_shotspeed;
use crate::pd_spike::pdmath::{baddtor, baddtor2};
use crate::pd_spike::tile_level::{CdObstacle, CdResult, Edge, PerimCyl, TileLevel};

/// `PLAYER_DEFAULT_FOV`.
pub const PLAYER_DEFAULT_FOV: f32 = 60.0;

/// What the walk collides with: the level as PD's collision tiles see it (the
/// range's boxes are converted to the same polygons, `Range::geom`) and every
/// other chr's perimeter cylinder (`CDTYPE_ALL`; the player's own is left out,
/// as `prop_set_perim_enabled(prop, false)` does).
///
/// SUBSTITUTION: PD tests only the tiles of the rooms the player is in or moves
/// through (`los_find_intersecting_rooms_exhaustive`, `bmove_find_entered_rooms_by_pos`);
/// every polygon is a candidate here, rejected by bounding box.
pub struct WalkEnv<'a> {
    pub level: &'a TileLevel,
    pub cyls: &'a [PerimCyl],
}

/// The collision results PD keeps in globals after a test that collided
/// (`cd_get_edge`, `cd_has_distance` / `cd_get_distance`). The obstacle prop
/// (`cd_get_obstacle_prop`) only matters for pushing, which a free-for-all never does.
#[derive(Clone, Copy, Debug, Default)]
struct CdGlobals {
    edge: Option<Edge>,
    dist: Option<f32>,
}

impl CdGlobals {
    fn set(&mut self, o: Option<CdObstacle>) {
        if let Some(o) = o {
            *self = CdGlobals { edge: Some(o.edge), dist: o.dist };
        }
    }
}

/// One tick's controls, already mapped from keys/mouse (or a pad).
#[derive(Clone, Debug, Default)]
pub struct PdInput {
    /// Mouse movement since the last tick, in pixels.
    pub mouse_dx: f32,
    pub mouse_dy: f32,
    /// Z / left mouse.
    pub fire: bool,
    /// R / right mouse (hold to aim).
    pub aim: bool,
    /// The movement stick, -127..127 (x right, y forward).
    pub walk_x: i32,
    pub walk_y: i32,
    /// Turn/look stick for pads, -80..80 like the N64 stick (x right, y up).
    pub look_x: i32,
    pub look_y: i32,
    /// B / use: held.
    pub use_held: bool,
    pub reload: bool,
    pub cycle_next: bool,
    pub cycle_prev: bool,
    pub select: Option<(i32, bool)>,
    /// Crouch key presses: go down one level / up one level.
    pub crouch_down: bool,
    pub crouch_up: bool,
    /// Manual zoom (C-up / C-down while aiming a sniper rifle or Farsight): held.
    pub zoom_in: bool,
    pub zoom_out: bool,
    /// An N64 pad drove this frame: PD's control style 1.1 (`bondmove.c:1166`).
    /// The stick is `look_x`/`look_y` (-80..80, +y up); walking comes from it,
    /// and the C-buttons strafe / look (or crouch / lean / zoom while aiming).
    pub pad: bool,
    pub c_up: bool,
    pub c_down: bool,
    pub c_left: bool,
    pub c_right: bool,
    /// A held (`invbuttons`): tap = next gun, A+Z = previous gun.
    pub a_held: bool,
}

/// `struct headanim` (`types.h:4549`), with `translateperframe` measured at reset.
#[derive(Clone, Copy, Debug)]
struct HeadAnim {
    animnum: u16,
    loopframe: f32,
    endframe: f32,
    translateperframe: f32,
    maxspeed: f32,
}

const HEADANIM_RESTING: i32 = 0;
const HEADANIM_MOVING: i32 = 1;

pub struct Player {
    /// `prop->pos`: the eye, `vv_manground` + the eye height.
    pub pos: Vec3,
    /// `vv_theta`, degrees; forward = (-sin, 0, cos).
    pub theta: f32,
    /// `vv_verta`, degrees, positive up.
    pub verta: f32,
    pub eyeheight: f32,
    /// `vv_headheight`: the top of the collision cylinder above `manground`
    /// (the eye height in multiplayer, `player.c:1495`).
    pub headheight: f32,
    /// `bond2.radius`.
    pub radius: f32,
    /// `vv_manground`: the smoothed height the feet are at.
    pub manground: f32,
    /// `vv_ground`: the floor under the cylinder this frame.
    pub ground: f32,
    /// `sumground`: the low-pass behind `manground` going up steps and ramps.
    pub sumground: f32,
    /// `bdeltapos.y`: the fall speed (cm per tick, negative down).
    pub fallspeed: f32,
    pub isfalling: bool,
    pub fallstart: i32,
    pub onladder: bool,
    pub ladderupdown: f32,
    laddernormal: Vec3,
    /// The floor polygon and its room (`floorroom`).
    pub floorpoly: Option<usize>,
    pub floorroom: Option<u16>,
    /// Landing: `crouchtime240`, `crouchfall`, `sumcrouch` (the dip after a fall).
    crouchtime240: i32,
    crouchfall: f32,
    sumcrouch: f32,
    /// `bondshotspeed`: the shove from being shot (`chr_damage`), in the same
    /// per-tick units as a bot's.
    pub shotspeed: Vec3,
    /// `bondprevpos`.
    pub prevpos: Vec3,
    /// `player_die(true)` was asked for this frame: fell for 4 s, or out of the world.
    pub die_request: bool,
    /// The fall speed of a landing this frame (for the footstep sounds).
    pub landed: Option<f32>,
    cd: CdGlobals,

    pub speedtheta: f32,
    pub speedthetacontrol: f32,
    pub speedverta: f32,
    pub speedforwards: f32,
    pub speedsideways: f32,
    pub speedstrafe: f32,
    pub speedgo: f32,
    pub speedboost: f32,
    pub speedmaxtime60: i32,
    pub gunspeed: f32,

    pub crouchoffset: f32,
    pub crouchspeed: f32,
    pub crouchoffsetreal: f32,
    pub crouchoffsetrealsmall: f32,
    pub crouchheight: f32,

    pub swaytarget: f32,
    pub swayoffset0: f32,
    pub swayoffset2: f32,

    // head-bob model (bondhead.c)
    head: Model,
    head_anim: Anim,
    headanims: [HeadAnim; 2],
    pub headanim: i32,
    headdamp: f32,
    headamplitude: f32,
    sideamplitude: f32,
    headwalkingtime60: i32,
    pub headpos: Vec3,
    pub headlook: Vec3,
    pub headup: Vec3,
    headpossum: Vec3,
    headlooksum: Vec3,
    headupsum: Vec3,
    resetheadpos: bool,
    resetheadrot: bool,
    standheight: f32,
    standfrac: f32,
    standlook: [Vec3; 2],
    standup: [Vec3; 2],
    standcnt: usize,

    // look / aim
    pub look: Vec3,
    pub up: Vec3,
    pub swivelpos: [f32; 2],
    pub usedowntime: i32,
    /// `invdowntime`: A held for this many ticks (-1 = consumed).
    pub invdowntime: i32,
    /// `aimtaptime`: R held this long (a short tap uncrouches), -1 = used.
    pub aimtaptime: i32,
    /// Last frame's C-up / C-down, for the crouch presses.
    prev_c_updown: [bool; 2],
    prev_fire: bool,
    pub waitforzrelease: bool,
    pub zoominfovy: f32,
    zoominfovyold: f32,
    zoominfovynew: f32,
    zoomintime: f32,
    zoomintimemax: f32,
    pub headroll: bool,

    pub mouse_sens: f32,
    pub mouseaimspeed: f32,
    pub crosshairsway: f32,
    pub crosshairedgeboundary: f32,

    bank: Arc<AnimBank>,
}

impl Player {
    pub fn new(bank: Arc<AnimBank>, head_anims: (u16, u16, u16), pos: Vec3, theta: f32) -> Self {
        let head = Model::new(Arc::new(player_head_modeldef()));
        let (walk, run, hold) = head_anims;
        let mut p = Player {
            pos: pos + Vec3::Y * 159.0,
            theta,
            verta: 0.0,
            eyeheight: 159.0,
            headheight: 159.0,
            radius: 30.0,
            manground: pos.y,
            ground: pos.y,
            sumground: pos.y / 0.045_499_98,
            // bwalk_init: bdeltapos.y = -0.0001.
            fallspeed: -0.0001,
            isfalling: false,
            fallstart: 0,
            onladder: false,
            ladderupdown: 0.0,
            laddernormal: Vec3::ZERO,
            floorpoly: None,
            floorroom: None,
            crouchtime240: 0,
            crouchfall: 0.0,
            sumcrouch: 0.0,
            shotspeed: Vec3::ZERO,
            prevpos: pos,
            die_request: false,
            landed: None,
            cd: CdGlobals::default(),
            speedtheta: 0.0,
            speedthetacontrol: 0.0,
            speedverta: 0.0,
            speedforwards: 0.0,
            speedsideways: 0.0,
            speedstrafe: 0.0,
            speedgo: 0.0,
            speedboost: 1.0,
            speedmaxtime60: 0,
            gunspeed: 0.0,
            crouchoffset: 0.0,
            crouchspeed: 0.0,
            crouchoffsetreal: 0.0,
            crouchoffsetrealsmall: 0.0,
            crouchheight: 0.0,
            swaytarget: 0.0,
            swayoffset0: 0.0,
            swayoffset2: 0.0,
            head,
            head_anim: Anim::default(),
            // g_HeadAnims (bondhead.c:14)
            headanims: [
                HeadAnim { animnum: walk, loopframe: 9.5, endframe: 27.0, translateperframe: 0.0, maxspeed: 1.5 },
                HeadAnim { animnum: run, loopframe: 7.5, endframe: 17.0, translateperframe: 0.0, maxspeed: 100.0 },
            ],
            headanim: HEADANIM_RESTING,
            headdamp: 0.93,
            headamplitude: 1.0,
            sideamplitude: 1.0,
            headwalkingtime60: 0,
            headpos: Vec3::ZERO,
            headlook: Vec3::ZERO,
            headup: Vec3::ZERO,
            headpossum: Vec3::ZERO,
            headlooksum: Vec3::new(0.0, 0.0, 14.285_716),
            headupsum: Vec3::new(0.0, 14.285_716, 0.0),
            resetheadpos: true,
            resetheadrot: true,
            standheight: 0.0,
            standfrac: 0.0,
            standlook: [Vec3::Z; 2],
            standup: [Vec3::Y; 2],
            standcnt: 0,
            look: Vec3::Z,
            up: Vec3::Y,
            swivelpos: [0.0; 2],
            usedowntime: 0,
            invdowntime: 0,
            aimtaptime: 0,
            prev_c_updown: [false; 2],
            prev_fire: false,
            waitforzrelease: false,
            zoominfovy: PLAYER_DEFAULT_FOV,
            zoominfovyold: PLAYER_DEFAULT_FOV,
            zoominfovynew: PLAYER_DEFAULT_FOV,
            zoomintime: 0.0,
            zoomintimemax: 0.0,
            headroll: true,
            mouse_sens: 2.5,
            mouseaimspeed: 0.7,
            crosshairsway: 1.0,
            crosshairedgeboundary: 0.7,
            bank,
        };
        p.bhead_reset(hold);
        p
    }

    fn ctx<'a>(bank: &'a AnimBank, ci: &'a mut super::anim::ChrInfo, merging: bool) -> AnimCtx<'a> {
        AnimCtx { bank, scale: 0.100_000_01, chrinfo: Some((ci, 0)), merging_enabled: merging }
    }

    /// `bhead_reset` (`bondheadreset.c:37`).
    fn bhead_reset(&mut self, hold_anim: u16) {
        let bank = self.bank.clone();
        self.head.scale = 0.100_000_01;
        self.head_anim = Anim::default();
        self.head_anim.set_play_speed(1.0, 0.0);
        // translateperframe: summed root-motion z over the loop window × 0.1.
        for ha in self.headanims.iter_mut() {
            let mut total = 0i32;
            if let Some(ad) = bank.get(ha.animnum) {
                let mut f = ha.loopframe as i32;
                while (f as f32) < ha.endframe {
                    total += ad.pos_angle_as_int(0, f).0[2] as i32;
                    f += 1;
                }
            }
            ha.translateperframe = (total as f32 * 0.100_000_01) / (ha.endframe - ha.loopframe);
        }
        // Measure standheight off ANIM_TWO_GUN_HOLD.
        {
            let mut ci = std::mem::take(&mut self.head.chrinfo);
            let mut ctx = Self::ctx(&bank, &mut ci, true);
            self.head_anim.set_animation(&mut ctx, hold_anim, false, 0.0, 0.5, 0.0);
            update_chr_info(&self.head_anim, &mut ci);
            self.head.chrinfo = ci;
        }
        self.head.set_matrices_with_anim(&Mat4::IDENTITY, Some(&self.head_anim), &bank, None);
        self.standheight = self.head.matrices[0].w_axis.y;
        let ha = self.headanims[self.headanim as usize];
        {
            let mut ci = std::mem::take(&mut self.head.chrinfo);
            let mut ctx = Self::ctx(&bank, &mut ci, true);
            self.head_anim.set_animation(&mut ctx, ha.animnum, false, ha.loopframe, 0.5, 0.0);
            self.head.chrinfo = ci;
        }
        self.head_anim.set_looping(ha.loopframe, 0.0);
        self.head_anim.set_end_frame(&bank, ha.endframe);
        self.head_anim.flipfunc = true;
        self.bhead_update_idle_roll_seeded(0.5, 0.5, 0.5, 0.5);
    }

    /// `bhead_update_idle_roll` (`bondhead.c:24`) with explicit randoms.
    fn bhead_update_idle_roll_seeded(&mut self, r0: f32, r1: f32, r2: f32, r3: f32) {
        let c = self.standcnt;
        self.standlook[c] = Vec3::new((r0 - 0.5) * 0.02, 0.0, 1.0);
        self.standup[c] = Vec3::new((r1 - 0.5) * 0.02, 1.0, 0.0);
        if c != 0 {
            self.standlook[c].y = r2 * 0.01;
            self.standup[c].z = r3 * -0.01;
        } else {
            self.standlook[c].y = r2 * -0.01;
            self.standup[c].z = r3 * 0.01;
        }
        self.standcnt = 1 - self.standcnt;
    }

    /// `bhead_adjust_animation` (`bondhead.c:259`).
    fn bhead_adjust_animation(&mut self, speed: f32) {
        let bank = self.bank.clone();
        let mut speed = speed * self.headanims[HEADANIM_MOVING as usize].translateperframe;
        for i in 0..2 {
            let ha = self.headanims[i];
            if ha.maxspeed * ha.translateperframe >= speed {
                let prev = self.headanim;
                if i as i32 != prev {
                    let mut startframe = 0.0;
                    if prev >= 0 {
                        let ph = self.headanims[prev as usize];
                        startframe = (self.head_anim.frame - ph.loopframe) / (ph.endframe - ph.loopframe);
                        startframe = ha.loopframe + (ha.endframe - ha.loopframe) * startframe;
                    }
                    let flip = self.head_anim.flip;
                    let mut ci = std::mem::take(&mut self.head.chrinfo);
                    let mut ctx = Self::ctx(&bank, &mut ci, true);
                    self.head_anim.set_animation(&mut ctx, ha.animnum, flip, startframe, 0.5, 12.0);
                    self.head.chrinfo = ci;
                    self.head_anim.set_looping(ha.loopframe, 0.0);
                    self.head_anim.set_end_frame(&bank, ha.endframe);
                    self.head_anim.flipfunc = true;
                    self.headanim = i as i32;
                }
                speed /= ha.translateperframe;
                self.head_anim.set_speed(speed * 0.5, 0.0);
                break;
            }
        }
    }

    /// `bhead_set_damp` (`bondhead.c:113`).
    fn bhead_set_damp(&mut self, headdamp: f32) {
        if headdamp != self.headdamp {
            let divisor = 1.0 - headdamp;
            self.headlooksum = self.headlooksum * (1.0 - self.headdamp) / divisor;
            self.headupsum = self.headupsum * (1.0 - self.headdamp) / divisor;
            self.headdamp = headdamp;
        }
    }

    /// `bhead_update` (`bondhead.c:129`).
    fn bhead_update(&mut self, speedforwards: f32, speedsideways: f32, lv: Lv, bondbreathing: f32, crouchpos: i32, rng: &mut crate::pd_spike::pdmath::Rng) {
        let bank = self.bank.clone();
        let mut headpos = Vec3::ZERO;
        let mut lookvel = Vec3::new(0.0, 0.0, 1.0);
        let mut upvel = Vec3::new(0.0, 1.0, 0.0);
        let mut animspeed = 0.0;
        let mut m0 = Mat4::IDENTITY;
        if self.head_anim.animnum != 0 && bank.num_frames(self.head_anim.animnum) > 0 {
            animspeed = self.head_anim.abs_speed();
            if self.headanim == HEADANIM_RESTING {
                self.headamplitude = if animspeed > 0.7 {
                    1.0
                } else if animspeed > 0.1 {
                    0.4 + (animspeed - 0.1) * 0.6 / 0.6
                } else {
                    0.4
                };
                self.sideamplitude = self.headamplitude;
            } else if self.headanim == HEADANIM_MOVING {
                self.headamplitude = 0.9;
                self.sideamplitude = 0.5;
            } else {
                self.headamplitude = 1.0;
                self.sideamplitude = 1.0;
            }
            let mut ci = std::mem::take(&mut self.head.chrinfo);
            {
                let mut ctx = Self::ctx(&bank, &mut ci, false);
                self.head_anim.tick_quarter(&mut ctx, lv.lvupdate240, true);
            }
            update_chr_info(&self.head_anim, &mut ci);
            self.head.chrinfo = ci;
            self.head.set_matrices_with_anim(&Mat4::IDENTITY, Some(&self.head_anim), &bank, None);
            m0 = self.head.matrices[0];
            // modelpos -= matrices[0].xz; model_set_root_position
            let mut modelpos = self.head.chrinfo.pos;
            modelpos.x -= m0.w_axis.x;
            modelpos.z -= m0.w_axis.z;
            let ci = &mut self.head.chrinfo;
            let diff = Vec3::new(modelpos.x - ci.pos.x, 0.0, modelpos.z - ci.pos.z);
            ci.pos = modelpos;
            ci.unk24 += diff;
            ci.unk34 += diff;
            ci.unk40 += diff;
            ci.unk4c += diff;
        }
        if animspeed > 0.0 {
            m0.w_axis.x += speedsideways;
            m0.w_axis.z *= speedforwards;
            if lv.lvupdate240 > 0 {
                m0.w_axis.x /= lv.lvupdate60freal;
                m0.w_axis.z /= lv.lvupdate60freal;
            }
            headpos.x = m0.w_axis.x * self.headamplitude;
            headpos.y = (m0.w_axis.y - self.standheight) * self.headamplitude + self.standheight;
            headpos.z = m0.w_axis.z * self.headamplitude;
            if self.headanim >= 0 {
                lookvel = Vec3::new(
                    m0.z_axis.x * self.sideamplitude,
                    m0.z_axis.y * self.headamplitude,
                    (m0.z_axis.z - 1.0) * self.headamplitude + 1.0,
                );
                upvel = Vec3::new(
                    m0.y_axis.x * self.headamplitude,
                    (m0.y_axis.y - 1.0) * self.headamplitude + 1.0,
                    m0.y_axis.z * self.headamplitude,
                );
                self.headwalkingtime60 += lv.lvupdate60;
                if self.headwalkingtime60 > 60 {
                    self.bhead_set_damp(0.982);
                } else {
                    self.bhead_set_damp(0.997_489_99);
                }
            } else {
                lookvel = m0.z_axis.truncate();
                upvel = m0.y_axis.truncate();
                self.bhead_set_damp(0.96);
            }
        } else {
            headpos = Vec3::new(0.0, self.standheight, 0.0);
            self.headwalkingtime60 = 0;
            self.bhead_set_damp(0.997_489_99);
            if crouchpos != CROUCHPOS_SQUAT {
                self.standfrac += (0.008_333_334 + 0.025_000_002 * bondbreathing) * lv.lvupdate60freal;
                if self.standfrac >= 1.0 {
                    let r = [rng.randomfrac(), rng.randomfrac(), rng.randomfrac(), rng.randomfrac()];
                    self.bhead_update_idle_roll_seeded(r[0], r[1], r[2], r[3]);
                    self.standfrac -= 1.0;
                }
                let c = self.standcnt;
                lookvel = (self.standlook[1 - c] - self.standlook[c]) * self.standfrac + self.standlook[c];
                lookvel.x *= 1.0 + 5.0 * bondbreathing;
                lookvel.y *= 1.0 + 5.0 * bondbreathing;
                upvel = (self.standup[1 - c] - self.standup[c]) * self.standfrac + self.standup[c];
                upvel.x *= 1.0 + 5.0 * bondbreathing;
                upvel.z *= 1.0 + 5.0 * bondbreathing;
            }
        }
        // bhead_update_pos
        if self.resetheadpos {
            self.headpossum = Vec3::new(0.0, headpos.y / 0.018_000_006, 0.0);
            self.resetheadpos = false;
        }
        for _ in 0..lv.lvupdate240 {
            self.headpossum = headpos + 0.982 * self.headpossum;
        }
        self.headpos = self.headpossum * 0.018_000_006;
        // bhead_update_rot
        if self.resetheadrot {
            self.headlooksum = lookvel / (1.0 - self.headdamp);
            self.headupsum = upvel / (1.0 - self.headdamp);
            self.resetheadrot = false;
        }
        for _ in 0..lv.lvupdate240 {
            self.headlooksum = lookvel + self.headdamp * self.headlooksum;
            self.headupsum = upvel + self.headdamp * self.headupsum;
        }
        self.headlook = self.headlooksum * (1.0 - self.headdamp);
        self.headup = self.headupsum * (1.0 - self.headdamp);
    }

    /// `bhead_get_breathing_value` (`bondhead.c:308`).
    fn bhead_get_breathing_value(&self, bondbreathing: f32) -> f32 {
        if self.headanim >= 0 {
            let a = bondbreathing * 0.012_500_001 + 1.0 / 240.0;
            let b = self.head_anim.abs_speed();
            if b > 0.0 {
                let ha = self.headanims[self.headanim as usize];
                let c = b / (ha.endframe - ha.loopframe);
                return c.max(a);
            }
            return a;
        }
        0.0
    }

    /// `bmove_get_speed_verta_limit` / theta control limit (`bondmove.c:329`).
    fn speed_limit(value: f32, fov: f32) -> f32 {
        if value > 0.0 {
            (fov * value * -0.7) / PLAYER_DEFAULT_FOV
        } else if value < 0.0 {
            (fov * -value * 0.7) / PLAYER_DEFAULT_FOV
        } else {
            0.0
        }
    }

    /// `bmove_update_speed_verta` / `bmove_update_speed_theta_control` (`:342`, `:397`).
    fn update_speed(cur: &mut f32, value: f32, fov: f32, lv60: f32) {
        let mult = fov / PLAYER_DEFAULT_FOV;
        let limit = Self::speed_limit(value, fov);
        if value > 0.0 {
            *cur -= if *cur > 0.0 { 0.05 } else { 0.0125 } * lv60 * mult;
            if *cur < limit {
                *cur = limit;
            }
        } else if value < 0.0 {
            *cur += if *cur < 0.0 { 0.05 } else { 0.0125 } * lv60 * mult;
            if *cur > limit {
                *cur = limit;
            }
        } else if *cur > limit {
            *cur -= 0.05 * lv60 * mult;
            if *cur < limit {
                *cur = limit;
            }
        } else {
            *cur += 0.05 * lv60 * mult;
            if *cur > limit {
                *cur = limit;
            }
        }
    }

    /// `player_tween_fov_y` + `player_update_zoom` (`player.c:2068`, `:2101`).
    fn tween_fov(&mut self, target: f32, lv60: f32) {
        let cur_target = if self.zoomintimemax > self.zoomintime { self.zoominfovynew } else { self.zoominfovy };
        if cur_target != target {
            self.zoomintime = 0.0;
            self.zoomintimemax = (self.zoominfovy - target).abs() * 15.0 / 30.0;
            self.zoominfovyold = self.zoominfovy;
            self.zoominfovynew = target;
        }
        if self.zoomintime < self.zoomintimemax {
            self.zoomintime = (self.zoomintime + lv60).min(self.zoomintimemax);
            self.zoominfovy =
                self.zoominfovyold + (self.zoomintime * (self.zoominfovynew - self.zoominfovyold)) / self.zoomintimemax;
        } else {
            self.zoomintime = self.zoomintimemax;
            self.zoominfovy = self.zoominfovynew;
        }
    }

    /// Stand the player on the floor at `ground` (`player_spawn` →
    /// `bwalk_init`): feet there, the eye above, nothing carried over.
    pub fn place(&mut self, ground: Vec3, theta: f32) {
        self.manground = ground.y;
        self.ground = ground.y;
        self.sumground = ground.y / 0.045_499_98;
        self.pos = Vec3::new(ground.x, ground.y + self.eyeheight, ground.z);
        self.prevpos = self.pos;
        self.theta = theta;
        self.verta = 0.0;
        self.fallspeed = -0.0001;
        self.isfalling = false;
        self.onladder = false;
        self.shotspeed = Vec3::ZERO;
        self.speedforwards = 0.0;
        self.speedsideways = 0.0;
        self.speedstrafe = 0.0;
        self.speedgo = 0.0;
        self.speedtheta = 0.0;
        self.speedthetacontrol = 0.0;
        self.speedverta = 0.0;
        self.crouchtime240 = 0;
        self.crouchfall = 0.0;
        self.sumcrouch = 0.0;
        self.crouchheight = 0.0;
        self.die_request = false;
        self.resetheadpos = true;
        self.resetheadrot = true;
    }

    /// Put the crouch straight at `offset` (−90 squat, −45 duck, 0 stand), as if
    /// `bwalk_update_crouch_offset` had finished getting there (a harness and
    /// spawn helper; PD tweens it).
    pub fn set_crouch_offset(&mut self, offset: f32) {
        self.crouchoffset = offset;
        self.crouchspeed = 0.0;
        self.update_crouch_offset_real();
    }

    /// `bmove_tick` for walk mode: input, the gun state machines, then walking
    /// (`bwalk_tick`, `bondwalk.c:1791`).
    pub fn tick(&mut self, input: &PdInput, bgun: &mut Bgun, lv: Lv, env: &WalkEnv) {
        self.die_request = false;
        self.landed = None;
        self.bmove_process_input(input, bgun, lv);
        // bwalk_update_prev_pos
        self.prevpos = self.pos;
        self.bwalk_update_theta(lv);
        self.bmove_update_look();
        self.bwalk_update_horizontal(bgun, lv, env);
        self.bwalk_update_vertical(lv, env);
    }

    /// `player_get_bbox` (`player.c:5193`), walk mode: radius, then the absolute
    /// top and bottom of the collision cylinder. The bottom is 30 cm above the feet,
    /// so anything lower is stepped onto rather than walked into.
    pub fn player_get_bbox(&self) -> (f32, f32, f32) {
        let ymin = self.manground + 30.0;
        let ymax = (self.manground + self.headheight + self.crouchoffsetrealsmall).max(self.manground + 80.0);
        (self.radius, ymax, ymin)
    }

    /// `player_update_perim_info` (`player.c:5160`): the cylinder other chrs
    /// collide with and shoot at.
    pub fn perim(&self) -> PerimCyl {
        let ymax = (self.manground + self.headheight + self.crouchoffsetrealsmall).max(self.manground + 80.0);
        PerimCyl { x: self.pos.x, z: self.pos.z, radius: self.radius, ymin: self.manground, ymax }
    }

    /// `bwalk_try_move_upwards` (`bondwalk.c:189`).
    fn bwalk_try_move_upwards(&mut self, env: &WalkEnv, amount: f32) -> CdResult {
        let newpos = self.pos + Vec3::Y * amount;
        let (radius, ymax, ymin) = self.player_get_bbox();
        let ymin = ymin - 0.1;
        let result = env.level.cd_test_volume_simple(newpos, radius, ymax - self.pos.y, ymin - self.pos.y, env.cyls);
        if result == CdResult::NoCollision {
            self.pos.y = newpos.y;
        }
        result
    }

    /// `bwalk_try_delta_nopush` (`bondwalk.c:236`): try moving by `delta`; on
    /// success (and `apply`) take the move and the turn.
    fn bwalk_try_delta_nopush(&mut self, env: &WalkEnv, delta: Vec3, rotateamount: f32, apply: bool, extrawidth: f32) -> CdResult {
        let mut result = CdResult::NoCollision;
        let mut dstpos = self.pos;
        if delta != Vec3::ZERO {
            dstpos += delta;
            let (radius, ymax, ymin) = self.player_get_bbox();
            let radius = radius + extrawidth;
            let (xdiff, zdiff) = (dstpos.x - self.pos.x, dstpos.z - self.pos.z);
            let halfradius = radius * 0.5;
            let (rymax, rymin) = (ymax - self.pos.y, ymin - self.pos.y);
            let level = env.level;
            if xdiff > halfradius || zdiff > halfradius || xdiff < -halfradius || zdiff < -halfradius {
                let (r, o) = level.cd_test_cylmove_oobfail_findclosest_finddist(self.pos, dstpos, radius, rymax, rymin, env.cyls);
                result = r;
                if r == CdResult::Error {
                    self.cd = CdGlobals::default();
                }
                self.cd.set(o);
                if result == CdResult::NoCollision {
                    let (r, o) = level.cd_test_volume_fromdir(self.pos, dstpos, radius, rymax, rymin, env.cyls);
                    result = r;
                    self.cd.set(o);
                }
            } else {
                let (r, o) = level.cd_test_volume_fromdir(self.pos, dstpos, radius, rymax, rymin, env.cyls);
                result = r;
                self.cd.set(o);
            }
        }
        if result == CdResult::NoCollision && apply {
            // BADRTOD4(rotateamount)
            let mut degrees = self.theta + rotateamount * 360.0 / crate::pd_spike::pdmath::M_BADTAU;
            while degrees < 0.0 {
                degrees += 360.0;
            }
            while degrees >= 360.0 {
                degrees -= 360.0;
            }
            self.theta = degrees;
            self.pos = dstpos;
        }
        result
    }

    /// `bwalk_try_delta` (`bondwalk.c:378`). Its obstacle reactions are doors
    /// (none here), pushable chrs (only teammates in multiplayer, and a
    /// free-for-all has none) and pushable objects (none), so it is the nopush
    /// test.
    fn bwalk_try_delta(&mut self, env: &WalkEnv, delta: Vec3, rotateamount: f32, apply: bool, extrawidth: f32) -> CdResult {
        self.bwalk_try_delta_nopush(env, delta, rotateamount, apply, extrawidth)
    }

    /// `bwalk_try_fulldelta` (`bondwalk.c:526`).
    fn bwalk_try_fulldelta(&mut self, env: &WalkEnv, delta: Vec3) -> (CdResult, Edge) {
        let result = self.bwalk_try_delta(env, delta, 0.0, true, 0.0);
        let edge = if result == CdResult::Collision { self.cd.edge.unwrap_or((self.pos, self.pos)) } else { (Vec3::ZERO, Vec3::ZERO) };
        (result, edge)
    }

    /// `bwalk_try_quarterdelta` (`bondwalk.c:541`): a quarter of the way to the
    /// obstacle the last test met. A collision with a *different* edge is
    /// reported (with that edge); anything else that fails is an error.
    fn bwalk_try_quarterdelta(&mut self, env: &WalkEnv, delta: Vec3, prevedge: Edge) -> (CdResult, Edge) {
        let mut edge = (Vec3::ZERO, Vec3::ZERO);
        if let Some(distance) = self.cd.dist {
            let quarter = delta * distance / 4.0;
            let result = self.bwalk_try_delta(env, quarter, 0.0, true, 0.0);
            if result == CdResult::NoCollision {
                return (CdResult::NoCollision, edge);
            }
            if result == CdResult::Collision {
                edge = self.cd.edge.unwrap_or(edge);
                if prevedge.0 != edge.0 || prevedge.1 != edge.1 {
                    return (CdResult::Collision, edge);
                }
            }
        }
        (CdResult::Error, edge)
    }

    /// `bwalk_try_slide_along_edge` (`bondwalk.c:588`): the move projected onto the edge.
    fn bwalk_try_slide_along_edge(&mut self, env: &WalkEnv, delta: Vec3, e: Edge) -> CdResult {
        let (v1, v2) = e;
        if v1.x != v2.x || v1.z != v2.z {
            let mut edgedir = Vec3::new(v2.x - v1.x, 0.0, v2.z - v1.z);
            let tmp = (edgedir.x * edgedir.x + edgedir.z * edgedir.z).sqrt();
            edgedir.x *= 1.0 / tmp;
            edgedir.z *= 1.0 / tmp;
            let tmp = delta.x * edgedir.x + delta.z * edgedir.z;
            let newdelta = Vec3::new(edgedir.x * tmp, 0.0, edgedir.z * tmp);
            return self.bwalk_try_delta(env, newdelta, 0.0, true, 0.0);
        }
        CdResult::Error
    }

    /// `bwalk_try_slide_along_corner` (`bondwalk.c:616`): if the destination is
    /// within a radius of one of the edge's ends, the move projected onto the
    /// perpendicular to (end − pos): rounding the corner.
    fn bwalk_try_slide_along_corner(&mut self, env: &WalkEnv, delta: Vec3, e: Edge) -> CdResult {
        let (radius, _, _) = self.player_get_bbox();
        let pos = self.pos;
        let round = |this: &mut Self, vtx: Vec3| -> Option<CdResult> {
            if vtx.x != pos.x || vtx.z != pos.z {
                let (mut x, mut z) = (-(vtx.z - pos.z), vtx.x - pos.x);
                let tmp = (x * x + z * z).sqrt();
                x *= 1.0 / tmp;
                z *= 1.0 / tmp;
                let tmp = delta.x * x + delta.z * z;
                let newdelta = Vec3::new(x * tmp, 0.0, z * tmp);
                if this.bwalk_try_delta(env, newdelta, 0.0, true, 0.0) == CdResult::NoCollision {
                    return Some(CdResult::NoCollision);
                }
            }
            None
        };
        let near = |vtx: Vec3| {
            let (x, z) = (vtx.x - (pos.x + delta.x), vtx.z - (pos.z + delta.z));
            x * x + z * z <= radius * radius
        };
        if near(e.0) {
            if let Some(r) = round(self, e.0) {
                return r;
            }
        } else if near(e.1) {
            if let Some(r) = round(self, e.1) {
                return r;
            }
        }
        CdResult::Collision
    }

    /// `bwalk_resolve_posdelta` (`bondwalk.c:1257`): the full move; else a
    /// quarter of the way to the obstacle, then slide along its edge or round its
    /// corner (only when not leaning); against a second, nearer obstacle, the
    /// same with both edges.
    fn bwalk_resolve_posdelta(&mut self, env: &WalkEnv, delta: Vec3, notrleaning: bool) {
        let (r, edge_a) = self.bwalk_try_fulldelta(env, delta);
        if r != CdResult::Collision {
            return;
        }
        let (result, edge_b) = self.bwalk_try_quarterdelta(env, delta, edge_a);
        if result != CdResult::Collision {
            if notrleaning && self.bwalk_try_slide_along_edge(env, delta, edge_a) != CdResult::NoCollision {
                self.bwalk_try_slide_along_corner(env, delta, edge_a);
            }
        } else {
            let (_, _edge_c) = self.bwalk_try_quarterdelta(env, delta, edge_b);
            if notrleaning
                && self.bwalk_try_slide_along_edge(env, delta, edge_b) != CdResult::NoCollision
                && self.bwalk_try_slide_along_edge(env, delta, edge_a) != CdResult::NoCollision
                && self.bwalk_try_slide_along_corner(env, delta, edge_b) != CdResult::NoCollision
            {
                self.bwalk_try_slide_along_corner(env, delta, edge_a);
            }
        }
    }

    /// `bmove_process_input` (`bondmove.c:695`), CONTROLMODE_PC + mouse.
    fn bmove_process_input(&mut self, input: &PdInput, bgun: &mut Bgun, lv: Lv) {
        let lv60 = lv.lvupdate60freal;
        let fov = self.zoominfovy;
        let mlookscale = if lv.lvupdate240 != 0 { 4.0 / lv.lvupdate240 as f32 } else { 4.0 };
        // inputMouseGetScaledDelta (pcport input.c:1256)
        let freelookdx = input.mouse_dx * (0.022 / 3.5) * self.mouse_sens;
        let freelookdy = input.mouse_dy * (0.022 / 3.5) * self.mouse_sens;

        // AIMCONTROL_HOLD: aim while R is held.
        bgun.p.insightaimmode = input.aim;
        let aiming = bgun.p.insightaimmode;
        let allowmcross = freelookdx != 0.0 || freelookdy != 0.0 || self.swivelpos[0] != 0.0 || self.swivelpos[1] != 0.0;

        let canswivelgun = !aiming;
        let canmanualaim = aiming;
        let pad = input.pad;
        // 1.1 (bondmove.c:1166): the stick walks (analogwalk = c1stickysafe) and
        // turns; strafing is digital on C-left/right, pitch digital on C-up/down.
        let (analogstrafe, analogwalk) = if pad {
            (0, if !aiming { input.look_y } else { 0 })
        } else if !aiming {
            (input.walk_x, input.walk_y)
        } else {
            (0, 0)
        };
        let unk14 = !pad && !aiming && (input.walk_x != 0 || input.walk_y != 0);
        let canlookahead = if pad { !aiming } else { !aiming && (input.walk_x != 0 || input.walk_y != 0) };
        let cannaturalpitch = !aiming && !pad;
        let digitalstep = if pad && !aiming { input.c_right as i32 - input.c_left as i32 } else { 0 };
        let cannaturalturn = !aiming;
        let mut speedvertadown = 0.0f32;
        let mut speedvertaup = 0.0f32;
        let mut aimturnleftspeed = 0.0f32;
        let mut aimturnrightspeed = 0.0f32;

        // C-up / C-down look (bondmove.c:1187; PD's non-inverted swap is folded
        // in, as in the aiming stick look below: C-up looks up).
        if pad && !aiming {
            if input.c_up {
                speedvertaup = 1.0;
            }
            if input.c_down {
                speedvertadown = 1.0;
            }
        }

        // Stick look while aiming (N64): push past 60 to turn.
        if aiming {
            let sy = -input.look_y;
            if sy > 60 {
                speedvertadown = ((sy - 60) as f32 / 10.0).min(1.0);
            } else if sy < -60 {
                speedvertaup = ((-60 - sy) as f32 / 10.0).min(1.0);
            }
            if input.look_x < -60 {
                aimturnleftspeed = ((-60 - input.look_x) as f32 / 10.0).min(1.0);
            } else if input.look_x > 60 {
                aimturnrightspeed = ((input.look_x - 60) as f32 / 10.0).min(1.0);
            }
        }
        // Mouse crosshair at the screen edge turns the view (pcport :1446).
        if aiming && allowmcross {
            let eb = self.crosshairedgeboundary;
            if self.swivelpos[0] > eb {
                aimturnrightspeed += (self.swivelpos[0] - eb) / (1.0 - eb);
            } else if self.swivelpos[0] < -eb {
                aimturnleftspeed += (self.swivelpos[0] + eb) / -(1.0 - eb);
            }
            if self.swivelpos[1] > eb {
                speedvertadown += (self.swivelpos[1] - eb) / (1.0 - eb);
            } else if self.swivelpos[1] < -eb {
                speedvertaup += (self.swivelpos[1] + eb) / -(1.0 - eb);
            }
        } else if !aiming {
            self.swivelpos = [0.0; 2];
        }

        // Crouch (C-down / C-up while aiming, or the crouch keys).
        let mut crouchdown = input.crouch_down as i32;
        let mut crouchup = input.crouch_up as i32;
        let manualzoom = bgun.gset.has_aim_flag(bgun.bgun_get_weapon_num(HAND_RIGHT), INVAIMFLAG_MANUALZOOM);
        if pad {
            // bondmove.c:1340: C-up/C-down presses while aiming crouch (the
            // zooming guns zoom instead, below); a short R tap uncrouches.
            let pressed_up = input.c_up && !self.prev_c_updown[0];
            let pressed_down = input.c_down && !self.prev_c_updown[1];
            if aiming && !manualzoom {
                if pressed_up {
                    if crouchdown > 0 {
                        crouchdown -= 1;
                    } else {
                        crouchup += 1;
                    }
                    self.aimtaptime = -1;
                }
                if pressed_down {
                    if crouchup > 0 {
                        crouchup -= 1;
                    } else {
                        crouchdown += 1;
                    }
                    self.aimtaptime = -1;
                }
            }
            // AIMCONTROL_HOLD (bondmove.c:1360).
            if aiming {
                if self.aimtaptime >= 0 {
                    self.aimtaptime += lv.lvupdate60;
                }
            } else {
                if self.aimtaptime > 0 && self.aimtaptime < 15 {
                    if crouchdown > 0 {
                        crouchdown -= 1;
                    } else {
                        crouchup += 1;
                    }
                }
                self.aimtaptime = 0;
            }
            self.prev_c_updown = [input.c_up, input.c_down];
        } else if aiming {
            if input.walk_y > 30 {
                crouchup += 1;
            }
            if input.walk_y < -30 {
                crouchdown += 1;
            }
        }
        // Lean: C-left/right while aiming (movedata.unk30/unk34), or A/D.
        let rleanleft = aiming && if pad { input.c_left } else { input.walk_x < -30 };
        let rleanright = aiming && if pad { input.c_right } else { input.walk_x > 30 };

        // B: hold 25 ticks to toggle the gun function; B+Z = temporary invert
        // (bondmove.c:1070, the N64 path).
        if input.use_held {
            if self.usedowntime >= -1 {
                if input.fire && self.usedowntime > -1 && bgun.bgun_consider_toggle_gun_function(self.usedowntime, true) != USETIMER_CONTINUE {
                    self.usedowntime = -3;
                }
                if self.usedowntime > -1 {
                    if self.usedowntime > 25 {
                        let r = bgun.bgun_consider_toggle_gun_function(self.usedowntime, false);
                        self.usedowntime = match r {
                            USETIMER_STOP => -1,
                            USETIMER_REPEAT => -2,
                            _ => self.usedowntime + 1,
                        };
                    } else {
                        self.usedowntime += 1;
                    }
                }
            } else if self.usedowntime >= -2 {
                bgun.bgun_consider_toggle_gun_function(self.usedowntime, false);
            }
        } else {
            // Released B after a short press: btapcount → bondactivateorreload
            // (bondmove.c:1303, lv.c:1293). Nothing in the range to activate, so
            // current_player_interact falls through to the reload.
            if self.usedowntime > 0 {
                bgun.bgun_reload_if_possible(HAND_RIGHT);
                bgun.bgun_reload_if_possible(HAND_LEFT);
            }
            self.usedowntime = 0;
            bgun.bgun_release_use();
        }

        // A (bondmove.c:1236): a tap cycles forward on release, A + Z steps
        // back; a hold past 15 ticks would open the active menu (not in the
        // range, so it just swallows the press).
        let fire_pressed = input.fire && !self.prev_fire;
        let mut weaponforward = false;
        let mut weaponback = false;
        if input.a_held {
            if self.invdowntime > -2 {
                if fire_pressed {
                    weaponback = true;
                    self.invdowntime = -1;
                }
                if self.invdowntime >= 0 && !input.fire {
                    if self.invdowntime > 15 {
                        self.invdowntime = -1;
                    } else {
                        self.invdowntime += lv.lvupdate60.max(1);
                    }
                }
            }
        } else {
            if self.invdowntime > 0 && !input.fire {
                weaponforward = true;
            }
            self.invdowntime = 0;
        }
        self.prev_fire = input.fire;
        if weaponforward {
            bgun.bgun_cycle(true);
        }
        if weaponback {
            bgun.bgun_cycle(false);
        }

        // Reload (the PC port's ALT1 / JO_ACTION_RELOAD).
        if input.reload {
            bgun.bgun_reload_if_possible(HAND_RIGHT);
            bgun.bgun_reload_if_possible(HAND_LEFT);
        }

        if self.waitforzrelease && !input.fire {
            self.waitforzrelease = false;
        }
        // Z doesn't fire while A is held (bondmove.c:1432).
        let triggeron = !self.waitforzrelease && input.fire && !input.a_held;

        bgun.bgun_tick_gameplay(triggeron, lv);

        // Manual zoom (bondmove.c:1320 → gset_zoom_out / gset_zoom_in,
        // bondmove.c:1480): C-down / C-up on the pad. PD's increment is 0.5 only
        // for a Farsight in the LEFT hand (its "@bug?"), so it is always 1 here.
        let weaponnum = bgun.bgun_get_weapon_num(HAND_RIGHT);
        if aiming && bgun.gset.has_aim_flag(weaponnum, INVAIMFLAG_MANUALZOOM) {
            if input.zoom_out || (pad && input.c_down) {
                bgun.gset_zoom_out(1.0, lv60);
            }
            if input.zoom_in || (pad && input.c_up) {
                bgun.gset_zoom_in(1.0, lv60);
            }
        }

        // Zoom (bondmove.c:1900).
        let mut zoomfov = PLAYER_DEFAULT_FOV;
        if aiming {
            zoomfov = bgun.gset_get_gun_zoom_fov();
        }
        if bgun.bgun_get_weapon_num(HAND_RIGHT) == WEAPON_AR34 && bgun.hands[HAND_RIGHT].weaponfunc == FUNC_SECONDARY {
            zoomfov = bgun.gset_get_gun_zoom_fov();
        }
        if zoomfov <= 0.0 {
            zoomfov = PLAYER_DEFAULT_FOV;
        }
        self.tween_fov(zoomfov, lv60);

        // bwalk_apply_move_data (bondwalk.c:1335)
        self.bwalk_apply_move_data(analogstrafe, analogwalk, digitalstep, unk14, canlookahead, rleanleft, rleanright, crouchdown, crouchup, bgun, lv);

        // Speed boost after 3 s of full forward.
        if self.speedmaxtime60 >= 180 {
            if self.speedboost < 1.25 {
                self.speedboost += 0.01 * lv60;
            }
            if self.speedboost > 1.25 {
                self.speedboost = 1.25;
            }
        } else {
            if self.speedboost > 1.0 {
                self.speedboost -= 0.01 * lv60;
            }
            if self.speedboost < 1.0 {
                self.speedboost = 1.0;
            }
        }

        // Pitch.
        if cannaturalpitch {
            let tmp = fov / PLAYER_DEFAULT_FOV;
            let mut f = (input.look_y as f32 / 70.0).clamp(-1.0, 1.0);
            f = if f >= 0.0 { f * f } else { -(f * f) };
            // Up on the stick / mouse is up: PD's default (non-inverted) pitch.
            let f = -f + freelookdy * mlookscale;
            self.speedverta = -f * tmp;
        } else if speedvertadown > 0.0 {
            Self::update_speed(&mut self.speedverta, speedvertadown, fov, lv60);
        } else if speedvertaup > 0.0 {
            Self::update_speed(&mut self.speedverta, -speedvertaup, fov, lv60);
        } else {
            Self::update_speed(&mut self.speedverta, 0.0, fov, lv60);
        }
        self.verta += self.speedverta * lv60 * 3.5;

        // Turn.
        if cannaturalturn {
            let tmp = fov / PLAYER_DEFAULT_FOV;
            let mut f = (input.look_x as f32 / 70.0).clamp(-1.0, 1.0);
            f = if f >= 0.0 { f * f } else { -(f * f) };
            f += freelookdx * mlookscale;
            self.speedthetacontrol = f * tmp;
        } else if aimturnleftspeed > 0.0 {
            Self::update_speed(&mut self.speedthetacontrol, aimturnleftspeed, fov, lv60);
        } else if aimturnrightspeed > 0.0 {
            Self::update_speed(&mut self.speedthetacontrol, -aimturnrightspeed, fov, lv60);
        } else {
            Self::update_speed(&mut self.speedthetacontrol, 0.0, fov, lv60);
        }
        self.speedtheta = self.speedthetacontrol;
        // bwalk_update_speed_theta
        if bgun.p.crouchpos == CROUCHPOS_SQUAT {
            self.speedtheta *= 0.5;
        } else if bgun.p.crouchpos == CROUCHPOS_DUCK {
            self.speedtheta *= 0.75;
        }

        // Weapon switching.
        if input.cycle_next {
            bgun.bgun_cycle(true);
        }
        if input.cycle_prev {
            bgun.bgun_cycle(false);
        }
        if let Some((w, dual)) = input.select {
            bgun.select_weapon(w, dual);
        }

        // Crosshair swivel.
        let swivel = [self.speedtheta * 0.3, -self.speedverta * 0.1];
        bgun.swivel_extra = swivel;
        if canswivelgun {
            // bmoveApplyCrosshairSwivel (pcport :442): mouse turning sways less.
            let mouse_active = freelookdx != 0.0 || freelookdy != 0.0;
            let joy_active = input.look_x != 0 || input.look_y != 0;
            let (xs, ys) = if mouse_active && joy_active {
                (self.crosshairsway * 0.8, self.crosshairsway * 0.8)
            } else if mouse_active {
                (self.crosshairsway * 0.2, self.crosshairsway * 0.3)
            } else {
                (self.crosshairsway, self.crosshairsway)
            };
            let x = self.speedtheta * 0.3 * xs;
            let y = -self.speedverta * 0.1 * ys;
            bgun.bgun_swivel_with_damp(x, y, 0.963);
        } else if canmanualaim {
            if allowmcross && (input.look_x == 0 && input.look_y == 0) {
                let xcoeff = 320.0 / 1080.0;
                let ycoeff = 240.0 / 1080.0;
                let xscale = (self.mouseaimspeed * xcoeff) / bgun.p.aspect;
                let yscale = self.mouseaimspeed * ycoeff;
                let x = (self.swivelpos[0] + freelookdx * xscale).clamp(-1.0, 1.0);
                let y = (self.swivelpos[1] + freelookdy * yscale).clamp(-1.0, 1.0);
                self.swivelpos = [x, y];
                bgun.bgun_swivel_with_damp(x, y, 0.01);
            } else {
                bgun.bgun_swivel_without_damp(input.look_x as f32 * 0.65 / 80.0, -input.look_y as f32 * 0.65 / 80.0);
            }
        }
    }

    /// `bwalk_apply_move_data` (`bondwalk.c:1335`).
    #[allow(clippy::too_many_arguments)]
    fn bwalk_apply_move_data(
        &mut self,
        analogstrafe: i32,
        analogwalk: i32,
        digitalstep: i32,
        unk14: bool,
        canlookahead: bool,
        rleanleft: bool,
        rleanright: bool,
        crouchdown: i32,
        crouchup: i32,
        bgun: &mut Bgun,
        lv: Lv,
    ) {
        let lv60 = lv.lvupdate60freal;
        // Sideways: digital (C-left/right) then analog (bondwalk.c:1339).
        if digitalstep < 0 {
            self.update_speed_sideways(-1.0, 0.2, lv.lvupdate60.max(1));
        } else if digitalstep > 0 {
            self.update_speed_sideways(1.0, 0.2, lv.lvupdate60.max(1));
        } else if !unk14 {
            self.update_speed_sideways(0.0, 0.2, lv.lvupdate60);
        }
        if unk14 {
            self.update_speed_sideways(analogstrafe as f32 * 0.014_285_714, 0.2, lv.lvupdate60);
        }
        // Forward/back
        if !canlookahead {
            self.update_speed_forwards(0.0, 1.0, lv60);
        } else {
            self.update_speed_forwards(analogwalk as f32 * 0.014_285_714, 1.0, lv60);
            if analogwalk > 60 {
                self.speedmaxtime60 += lv.lvupdate60;
            } else {
                self.speedmaxtime60 = 0;
            }
        }
        self.speedforwards = self.speedforwards.clamp(-1.0, 1.0);
        self.speedsideways = self.speedsideways.clamp(-1.0, 1.0);
        self.speedforwards *= 1.08;
        self.speedforwards *= self.speedboost;
        if !canlookahead || bgun.p.crouchpos != CROUCHPOS_STAND {
            self.speedmaxtime60 = 0;
        }
        // bwalk_set_sway_target: lean
        self.swaytarget = if rleanleft {
            -75.0
        } else if rleanright {
            75.0
        } else {
            0.0
        };
        for _ in 0..crouchdown {
            bgun.p.crouchpos = (bgun.p.crouchpos - 1).max(CROUCHPOS_SQUAT);
        }
        for _ in 0..crouchup {
            bgun.p.crouchpos = (bgun.p.crouchpos + 1).min(CROUCHPOS_STAND);
        }
    }

    /// `bwalk_update_speed_sideways` (`bondwalk.c:693`).
    fn update_speed_sideways(&mut self, targetspeed: f32, accelspeed: f32, mult: i32) {
        let step = accelspeed * mult as f32;
        if self.speedstrafe > targetspeed {
            self.speedstrafe = (self.speedstrafe - step).max(targetspeed);
        } else if self.speedstrafe < targetspeed {
            self.speedstrafe = (self.speedstrafe + step).min(targetspeed);
        }
        self.speedsideways = self.speedstrafe;
    }

    /// `bwalk_update_speed_forwards` (`bondwalk.c:716`).
    fn update_speed_forwards(&mut self, targetspeed: f32, accelspeed: f32, lv60: f32) {
        if self.speedgo < targetspeed {
            self.speedgo = (self.speedgo + accelspeed * lv60).min(targetspeed);
        } else if self.speedgo > targetspeed {
            self.speedgo = (self.speedgo - accelspeed * lv60).max(targetspeed);
        }
        self.speedforwards = self.speedgo;
    }

    /// `bwalk_update_theta` (`bondwalk.c:1239`).
    fn bwalk_update_theta(&mut self, lv: Lv) {
        let mult = 159.0 / self.eyeheight;
        let rotateamount = self.speedtheta * mult * lv.lvupdate60freal * 0.017_450_513 * 3.5;
        let mut degrees = self.theta + rotateamount * 360.0 / crate::pd_spike::pdmath::M_BADTAU;
        while degrees < 0.0 {
            degrees += 360.0;
        }
        while degrees >= 360.0 {
            degrees -= 360.0;
        }
        self.theta = degrees;
    }

    /// `bmove_update_look` (`bondmove.c:1976`).
    fn bmove_update_look(&mut self) {
        while self.verta < -180.0 {
            self.verta += 360.0;
        }
        while self.verta >= 180.0 {
            self.verta -= 360.0;
        }
        self.verta = self.verta.clamp(-90.0, 90.0);
    }

    /// `bond2.theta`: the facing unit vector.
    pub fn theta_vec(&self) -> Vec3 {
        let t = baddtor2(self.theta);
        Vec3::new(-t.sin(), 0.0, t.cos())
    }

    /// `bwalk_update_crouch_offset_real` (`bondwalk.c:1180`).
    fn update_crouch_offset_real(&mut self) {
        let e = self.eyeheight;
        if e + -90.0 * e * (1.0 / 159.0) < 69.0 {
            self.crouchoffsetreal = self.crouchoffset * ((69.0 - e) / -90.0);
        } else {
            self.crouchoffsetreal = self.crouchoffset * e * (1.0 / 159.0);
        }
        self.crouchoffsetrealsmall = self.crouchoffsetreal;
    }

    /// `bwalk_update_crouch_offset` (`bondwalk.c:1197`) with `apply_speed`. Rising
    /// into a ceiling is refused: the offset stays and the crouch goes down a level.
    fn update_crouch_offset(&mut self, bgun: &mut Bgun, lv: Lv, env: &WalkEnv) {
        let target = match bgun.p.crouchpos {
            CROUCHPOS_SQUAT => -90.0,
            CROUCHPOS_DUCK => -45.0,
            _ => 0.0,
        };
        if target != self.crouchoffset {
            let prev = (self.crouchoffset, self.crouchoffsetreal, self.crouchoffsetrealsmall);
            // apply_speed(&crouchoffset, target, &crouchspeed, 0.5, 0.5, 5.0)
            let (accel, decel, maxspeed) = (0.5f32, 0.5f32, 5.0f32);
            let mut speed = self.crouchspeed;
            for _ in 0..lv.lvupdate60 {
                let limit = speed * speed * 0.5 / decel;
                let rem = target - self.crouchoffset;
                if rem > 0.0 {
                    if speed > 0.0 && rem <= limit {
                        speed = (speed - decel).max(decel);
                    } else if speed < maxspeed {
                        speed += if speed < 0.0 { decel } else { accel };
                        speed = speed.min(maxspeed);
                    }
                    if speed >= rem {
                        self.crouchoffset = target;
                        break;
                    }
                    self.crouchoffset += speed;
                } else {
                    if speed < 0.0 && -rem <= limit {
                        speed = (speed + decel).min(-decel);
                    } else if speed > -maxspeed {
                        speed -= if speed > 0.0 { decel } else { accel };
                        speed = speed.max(-maxspeed);
                    }
                    if speed <= rem {
                        self.crouchoffset = target;
                        break;
                    }
                    self.crouchoffset += speed;
                }
            }
            self.crouchspeed = speed;
            self.update_crouch_offset_real();
            if self.bwalk_try_move_upwards(env, 0.0) == CdResult::Collision {
                // Crouch adjustment is blocked by the ceiling; bwalk_adjust_crouch_pos(-1).
                (self.crouchoffset, self.crouchoffsetreal, self.crouchoffsetrealsmall) = prev;
                self.crouchspeed = 0.0;
                bgun.p.crouchpos = (bgun.p.crouchpos - 1).clamp(CROUCHPOS_SQUAT, CROUCHPOS_STAND);
            }
        }
        if target == self.crouchoffset {
            self.crouchspeed = 0.0;
        }
        bgun.p.guncloseroffset = self.crouchoffset / -90.0;
    }

    /// `bwalk_update_horizontal` (`bondwalk.c:1425`).
    fn bwalk_update_horizontal(&mut self, bgun: &mut Bgun, lv: Lv, env: &WalkEnv) {
        let lv60 = lv.lvupdate60freal;
        let spc0 = (self.eyeheight - 159.0) / 353.333_3 + 1.0;
        // bwalk_apply_crouch_speed
        match bgun.p.crouchpos {
            CROUCHPOS_DUCK => {
                self.speedforwards *= 0.5;
                self.speedsideways *= 0.5;
            }
            CROUCHPOS_SQUAT => {
                self.speedforwards *= 0.35;
                self.speedsideways *= 0.35;
            }
            _ => {}
        }
        self.update_crouch_offset(bgun, lv, env);

        // bmove_shotspeed_to_lateral (bondmove.c:1877): being shot shoves.
        let (sintheta, costheta) = baddtor2(self.theta).sin_cos();
        let (shotforwards, shotsideways) = if self.shotspeed.x != 0.0 || self.shotspeed.z != 0.0 {
            bmove_dampen_shotspeed(&mut self.shotspeed, lv60);
            (
                self.shotspeed.z * costheta + -self.shotspeed.x * sintheta,
                -self.shotspeed.x * costheta - self.shotspeed.z * sintheta,
            )
        } else {
            (0.0, 0.0)
        };

        let th = self.theta_vec();
        let mut tmp1 = -self.swaytarget * th.z * spc0;
        let mut tmp2 = self.swaytarget * th.x * spc0;
        if self.crouchoffset < -45.0 {
            tmp1 *= 0.35;
            tmp2 *= 0.35;
        } else if self.crouchoffset < 0.0 {
            tmp1 *= 0.5;
            tmp2 *= 0.5;
        }
        let mut spb4 = tmp1 - self.swayoffset0;
        let mut spb0 = tmp2 - self.swayoffset2;
        let dist = (spb4 * spb4 + spb0 * spb0).sqrt();
        let (lv60f, lv240) = if lv60 > 4.0 { (4.0, 4) } else { (lv60, lv.lvupdate60) };
        let mut spa8 = 0.0f32;
        for _ in 0..lv240 {
            spa8 += (dist - spa8) * 0.1;
        }
        spa8 += 3.75 * lv60f;
        if self.crouchoffset < -45.0 {
            spa8 *= 0.35;
        } else if self.crouchoffset < 0.0 {
            spa8 *= 0.5;
        }
        if spa8 < dist {
            spa8 /= dist;
            spb4 *= spa8;
            spb0 *= spa8;
        }

        let speedsideways = ((self.speedsideways + shotsideways) * 0.8).abs();
        let speedforwards = (self.speedforwards + shotforwards).abs();
        let speedtheta = (self.speedtheta * 0.8).abs();
        let mut heartrate = speedforwards.max(speedsideways).max(speedtheta);
        if dist >= 0.1 && heartrate < 0.8 {
            heartrate = 0.8;
        }
        let br = &mut bgun.p.bondbreathing;
        if heartrate >= 0.75 {
            *br += (heartrate - 0.75) * lv60 / 900.0;
        } else {
            *br -= (0.75 - heartrate) * lv60 / 2700.0;
        }
        *br = br.clamp(0.0, 1.0);
        let bondbreathing = *br;

        let mult = self.headanims[HEADANIM_MOVING as usize].translateperframe * 0.5 * lv60;
        let spe0 = (self.speedsideways * spc0 + shotsideways) * mult;
        // bmove_update_head (bondmove.c:2071)
        let fwd = self.speedforwards * spc0 + shotforwards;
        self.bhead_adjust_animation(heartrate);
        let newspeedforwards = if heartrate != 0.0 { fwd / heartrate } else { 0.0 };
        let crouchpos = bgun.p.crouchpos;
        self.bhead_update(newspeedforwards, spe0, lv, bondbreathing, crouchpos, &mut bgun.rng);
        self.update_camera_basis();

        self.gunspeed = heartrate;
        let spdc = self.headpos.x;
        let spd8 = self.headpos.z;
        let mut spcc = Vec3::ZERO;
        spcc.x += (spd8 * th.x - spdc * th.z) * lv60;
        spcc.z += (spd8 * th.z + spdc * th.x) * lv60;
        spcc.x += spb4;
        spcc.z += spb0;

        // Ladders (bondwalk.c:1614): the part of the move into the ladder becomes
        // climbing (ladderupdown, applied by bwalk_update_vertical) and the rest
        // is slowed to 30 %.
        if self.onladder {
            let n = self.laddernormal.normalize_or_zero();
            self.laddernormal = n;
            let sp74 = -(spcc.x * n.x + spcc.z * n.z);
            if -4.0 * lv60 < sp74 {
                if sp74 < 0.0 {
                    spcc.x += sp74 * n.x;
                    spcc.z += sp74 * n.z;
                    self.ladderupdown = sp74 * 0.3;
                } else {
                    let (radius, ymax, _) = self.player_get_bbox();
                    if env
                        .level
                        .cd_find_ladder(self.pos, radius * 1.1, ymax - self.pos.y, self.manground - self.pos.y + 1.0)
                        .is_none()
                    {
                        self.ladderupdown = 0.0;
                    } else {
                        spcc.x += sp74 * n.x;
                        spcc.z += sp74 * n.z;
                        self.ladderupdown = sp74 * 0.3;
                    }
                }
                spcc.x *= 0.3;
                spcc.z *= 0.3;
            } else {
                self.ladderupdown = 0.0;
            }
        }

        let before = self.pos;
        self.bwalk_resolve_posdelta(env, spcc, self.swaytarget == 0.0);
        // Deltas from bondprevpos (bwalk_tick's start; the turn doesn't move).
        let xdelta = self.pos.x - self.prevpos.x;
        let zdelta = self.pos.z - self.prevpos.z;
        // A blocked shove loses the blocked part (bondwalk.c:1700).
        for (k, (d, want)) in [(xdelta, spcc.x), (zdelta, spcc.z)].into_iter().enumerate() {
            let a = if k == 0 { 0 } else { 2 };
            let s = &mut self.shotspeed[a];
            if d >= 0.0 {
                if *s > 0.0 {
                    if want >= 0.0 && d < want {
                        *s *= d / want;
                    }
                } else if want < 0.0 {
                    *s = 0.0;
                }
            } else if *s < 0.0 {
                if want <= 0.0 && want < d {
                    *s *= d / want;
                }
            } else if want > 0.0 {
                *s = 0.0;
            }
        }
        let (xdelta, zdelta) = (self.pos.x - before.x, self.pos.z - before.z);
        // Blocked movement bleeds speed (bondwalk.c:1736).
        let sp54 = -xdelta * th.z + zdelta * th.x;
        let sp50 = xdelta * th.x + zdelta * th.z;
        let sp4c = -spcc.x * th.z + spcc.z * th.x;
        let sp48 = spcc.x * th.x + spcc.z * th.z;
        if sp4c != 0.0 && self.speedstrafe * sp4c > 0.0 {
            let r = sp54 / sp4c;
            if r <= 0.0 {
                self.speedstrafe = 0.0;
            } else if r < 1.0 {
                self.speedstrafe *= r;
            }
        }
        if sp48 != 0.0 && self.speedgo * sp48 > 0.0 {
            let r = sp50 / sp48;
            if r <= 0.0 {
                self.speedgo = 0.0;
            } else if r < 1.0 {
                self.speedgo *= r;
            }
        }
        let f0 = spcc.x * spcc.x + spcc.z * spcc.z;
        let f0 = if f0 != 0.0 { ((xdelta * xdelta + zdelta * zdelta) / f0).sqrt() } else { 0.0 };
        self.swayoffset0 += f0 * spb4;
        self.swayoffset2 += f0 * spb0;

        // The gun's sway inputs (bondwalk.c:1771).
        let sp44 = self.speedtheta;
        let sp40 = (self.speedverta / 0.7 + self.crouchspeed / 5.0).clamp(-1.0, 1.0);
        let sp3c = self.gunspeed;
        let mut breathing = self.bhead_get_breathing_value(bondbreathing);
        if self.headanim == HEADANIM_MOVING {
            breathing *= 1.2;
        }
        bgun.bgun_update_sway(breathing, sp3c, sp40, sp44, 0.0);
        let mut v360 = self.verta;
        if v360 < 0.0 {
            v360 += 360.0;
        }
        bgun.bgun_set_adjust_pos(v360 * 0.017_450_513);
    }

    /// `bwalk_update_vertical` (`bondwalk.c:739`), NTSC final: ladders, the
    /// ground under the cylinder, stepping up (a low-pass on `manground`, at most
    /// 50 cm below the ground, refused by a ceiling), falling (PD's gravity, landing
    /// exactly on the ground), the landing dip, and the eye height.
    ///
    /// Not ported: lifts and escalators (Complex has none), `GEOFLAG_DIE` floors
    /// (not in the generic input), the counter-op radius fix, landing on a chr in a
    /// lift, and turbo mode.
    fn bwalk_update_vertical(&mut self, lv: Lv, env: &WalkEnv) {
        let level = env.level;
        let (radius, ymax, _ymin) = self.player_get_bbox();
        let lv60 = lv.lvupdate60freal;

        // On a ladder? (A second, lower probe keeps the player "near" one without
        // taking the ground from under it: onladder2.)
        let mut onladder = level.cd_find_ladder(self.pos, radius * 1.2, ymax - self.pos.y, self.manground - self.pos.y + 1.0);
        let mut onladder2 = false;
        if onladder.is_none() {
            if let Some(n) = level.cd_find_ladder(self.pos, radius * 1.1, ymax - self.pos.y, self.manground - self.pos.y - 10.0) {
                onladder2 = true;
                self.laddernormal = n;
            }
        }
        if let Some(n) = onladder {
            self.laddernormal = n;
        }

        // cd_find_ground_at_cyl_ctfril from the eye.
        let (mut ground, floorpoly) = level.cd_find_ground_at_cyl(self.pos, self.radius);
        self.floorpoly = floorpoly;
        self.floorroom = floorpoly.and_then(|p| level.geom.polys[p].room);
        if ground < -30000.0 {
            ground = -30000.0;
        }

        // Ladders
        if self.onladder {
            if self.ladderupdown >= 0.0 || (ground <= self.manground && ground <= self.manground + self.ladderupdown) {
                // Still on the ladder.
                if self.bwalk_try_move_upwards(env, self.ladderupdown) == CdResult::NoCollision {
                    self.manground += self.ladderupdown;
                }
            } else if self.bwalk_try_move_upwards(env, ground - self.manground) == CdResult::NoCollision {
                self.manground = ground;
                onladder = None;
            }
        }
        self.onladder = onladder.is_some();
        if self.onladder {
            self.ground = self.manground;
        } else if !onladder2 {
            self.ground = ground;
        }

        // Standing on flat ground, or going up stairs, ledges or ramps.
        if self.fallspeed >= 0.0 || self.ground > self.manground {
            self.sumground = self.manground / 0.045_499_98;
            for _ in 0..lv.lvupdate240 {
                self.sumground = self.sumground * 0.9545 + self.ground;
            }
            if self.manground < self.ground {
                // Feet are lower than the ground.
                let sumground = (self.sumground * 0.045_499_98).max(self.ground - 50.0);
                if self.bwalk_try_move_upwards(env, sumground - self.manground) == CdResult::NoCollision {
                    self.manground = sumground;
                }
            }
        }

        if self.manground > self.ground {
            // Not standing on the ground: falling.
            let fallspeed = self.fallspeed;
            let mut newmanground = self.manground;
            let multiplier = 0.277_777_777;
            let mut newfallspeed = fallspeed - lv60 * multiplier;
            newmanground += lv60 * (fallspeed + newfallspeed) * 0.5;
            let mut fallspeed = newfallspeed;
            if newmanground < self.ground {
                newfallspeed = self.manground - self.ground;
                newmanground = self.ground;
                fallspeed = -(self.fallspeed * self.fallspeed + (((newfallspeed + newfallspeed) * 0.277_777_777) / 60.0) * 60.0).sqrt();
            }
            if self.bwalk_try_move_upwards(env, newmanground - self.manground) == CdResult::NoCollision {
                self.manground = newmanground;
                self.fallspeed = fallspeed;
                if !self.isfalling {
                    self.isfalling = true;
                    self.fallstart = lv.lvframe60;
                } else if lv.lvframe60 - self.fallstart > 240 {
                    // Falling for 4 seconds.
                    self.die_request = true;
                }
            } else {
                // Not falling (something is in the way).
                self.fallspeed = 0.0;
                self.isfalling = false;
                if self.manground <= -30000.0 {
                    self.die_request = true;
                }
            }
        } else {
            self.isfalling = false;
            if self.manground <= -30000.0 {
                self.die_request = true;
            }
        }

        if self.fallspeed < 0.0 && self.manground <= self.ground {
            // Landing after a fall: the harder, the deeper the dip.
            self.isfalling = false;
            if self.fallspeed < -13.333_333 {
                self.crouchtime240 = 60;
                self.crouchfall = -90.0;
            } else if self.fallspeed < -5.0 {
                self.crouchtime240 = 60;
                self.crouchfall = (-5.0 - self.fallspeed) * -90.0 / 8.333_333;
            }
            if self.fallspeed < -6.0 {
                self.landed = Some(self.fallspeed);
            }
            self.fallspeed = 0.0;
        }

        // The dip: hold crouchfall for crouchtime240, then let it back to 0.
        for _ in 0..lv.lvupdate240 {
            if self.crouchtime240 > 0 {
                self.sumcrouch = self.sumcrouch * 0.9456 + self.crouchfall;
                self.crouchtime240 -= 1;
            } else {
                if self.crouchfall < 0.0 {
                    self.crouchfall -= -1.125;
                    if self.crouchfall >= 0.0 {
                        self.crouchfall = 0.0;
                    }
                }
                self.sumcrouch = self.sumcrouch * 0.9456 + self.crouchfall;
            }
        }

        self.crouchheight = self.sumcrouch * 0.054_400_027;
        let vv_height = (self.headpos.y / self.standheight) * self.eyeheight;
        let mut eyeheight = vv_height + self.crouchoffsetrealsmall + self.crouchheight * self.eyeheight * 0.006_289_308;
        if eyeheight < 30.0 {
            eyeheight = 30.0;
        }
        self.pos.y = self.manground + eyeheight;
        if self.pos.y < self.ground + 10.0 {
            self.pos.y = self.ground + 10.0;
        }
    }

    /// `bmove_update_head_with_mtx`'s camera basis (`bondmove.c:2098`).
    fn update_camera_basis(&mut self) {
        let mut v360 = self.verta;
        if v360 < 0.0 {
            v360 += 360.0;
        }
        let mut sp180 = pdmtx::load_x_rotation(baddtor2(360.0 - v360));
        if self.headroll {
            let sp116 = pdmtx::look_at_basis(Vec3::ZERO, -self.headlook, self.headup);
            sp180 = sp116 * sp180;
        }
        let sp116 = pdmtx::load_y_rotation(baddtor2(360.0 - self.theta));
        sp180 = sp116 * sp180;
        self.look = sp180.z_axis.truncate();
        self.up = sp180.y_axis.truncate();
    }

    /// The camera for this frame: `player_allocate_matrices`' `mtxf0068`
    /// (camera → world, `cam_get_projection_mtxf`) and `mtxf0064` (world → camera).
    pub fn camera(&self) -> (Mat4, Mat4) {
        let proj = pdmtx::look_basis(self.pos, self.look, self.up);
        let view = pdmtx::view_matrix(self.pos, self.look, self.up);
        (proj, view)
    }

    /// For the HUD/debug: which head clip is running and its speed.
    pub fn head_debug(&self) -> (i32, f32, f32) {
        (self.headanim, self.head_anim.frame, self.head_anim.speed)
    }

    /// Silence unused warnings for fields kept for parity.
    pub fn _unused(&self) -> f32 {
        baddtor(0.0)
    }
}
