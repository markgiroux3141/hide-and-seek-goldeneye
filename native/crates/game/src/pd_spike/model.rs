//! Perfect Dark's model animation state (`struct anim` + `lib/model.c`), ported
//! function by function, and the pose evaluation that turns it into joint matrices.
//!
//! The simulation only ever touches [`Anim`] — frame numbers, speeds, the merge —
//! so it runs headless with no clip data at all. [`ClipBank`] + [`evaluate`] are
//! the render half: they sample the exported glTF clips at the frames `Anim`
//! names and apply PD's per-joint callback (`chr_handle_joint_positioned`).
//!
//! What PD does that is easy to get wrong, all ported as written:
//!
//! * **Frames are a float that only ever grows (or shrinks) by `playspeed × speed ×
//!   0.25` per 240 Hz sub-tick** (`model_tick_anim_quarter_speed`, `model.c:2515`).
//!   Wrapping is done lazily by `model_constrain_or_wrap_anim_frame` whenever the
//!   frame is *set*, and only for clips carrying `ANIMFLAG_LOOP`; others clamp.
//! * **"Looping" is a separate, explicit mode** (`model_set_anim_looping`): on
//!   reaching the end it restarts at `loopframe` *through a merge* of `loopmerge`
//!   ticks. This is how the idle breathing loops (e.g. `ANIM_0002` frames 35-40)
//!   work — they are not loop-flagged clips.
//! * **The merge** (`model_copy_anim_for_merge` + `model_set_animation2`): changing
//!   animation snapshots the old one into the `*2` slot, which *keeps advancing*,
//!   and joint rotations are slerped from old to new as `fracmerge` falls 1 → 0
//!   over `timemerge` ticks (`model.c:787-800`). Translations are not merged.
//! * **Negative speed plays backwards** — the reversed-run backpedal is just this.
//!
//! Not ported: `flip` (bots never mirror), cutscene frame snapping, the
//! `ANIMFLAG_ABSOLUTETRANSLATION` branches (no bot animation has the flag), and
//! `model_set_anim_frame2_with_chr_stuff`'s root-motion bookkeeping — for a bot the
//! animation's horizontal travel is overwritten by `bot_update_lateral` anyway
//! (`chr.c:612`), so only its vertical component survives, which [`evaluate`] keeps.

use glam::{Mat4, Quat, Vec3};

use engine::skeletal::clip::AnimationClip;
use engine::skeletal::Skeleton;

use super::anims::{self, AnimId};

/// `struct anim`, the fields a bot's model uses.
#[derive(Clone, Debug)]
pub struct Anim {
    pub animnum: Option<AnimId>,
    pub frame: f32,
    pub framea: i32,
    pub frameb: i32,
    pub frac: f32,
    pub speed: f32,
    pub endframe: f32,
    pub looping: bool,
    pub loopframe: f32,
    pub loopmerge: f32,
    pub playspeed: f32,
    pub timespeed: f32,
    pub elapsespeed: f32,
    pub oldspeed: f32,
    pub newspeed: f32,

    pub animnum2: Option<AnimId>,
    pub frame2: f32,
    pub frame2a: i32,
    pub frame2b: i32,
    pub frac2: f32,
    pub speed2: f32,
    pub endframe2: f32,
    pub timespeed2: f32,
    pub elapsespeed2: f32,
    pub oldspeed2: f32,
    pub newspeed2: f32,

    pub timemerge: f32,
    pub elapsemerge: f32,
    pub fracmerge: f32,
}

impl Default for Anim {
    fn default() -> Self {
        Anim {
            animnum: None,
            frame: 0.0,
            framea: 0,
            frameb: 0,
            frac: 0.0,
            speed: 1.0,
            endframe: -1.0,
            looping: false,
            loopframe: 0.0,
            loopmerge: 0.0,
            playspeed: 1.0,
            timespeed: 0.0,
            elapsespeed: 0.0,
            oldspeed: 0.0,
            newspeed: 0.0,
            animnum2: None,
            frame2: 0.0,
            frame2a: 0,
            frame2b: 0,
            frac2: 0.0,
            speed2: 1.0,
            endframe2: -1.0,
            timespeed2: 0.0,
            elapsespeed2: 0.0,
            oldspeed2: 0.0,
            newspeed2: 0.0,
            timemerge: 0.0,
            elapsemerge: 0.0,
            fracmerge: 0.0,
        }
    }
}

/// `model_constrain_or_wrap_anim_frame` (`model.c:1712`).
fn constrain_or_wrap(frame: i32, anim: AnimId, endframe: f32) -> i32 {
    let info = anims::info(anim);
    let n = info.num_frames as i32;
    if frame < 0 {
        if info.looped {
            // PD's expression, including its quirk: a multiple of `n` maps to `n`,
            // one past the last frame. The sampler wraps that back to 0.
            n - (-frame % n)
        } else {
            0
        }
    } else if endframe >= 0.0 && frame > endframe as i32 {
        endframe.ceil() as i32
    } else if frame >= n {
        if info.looped {
            frame % n
        } else {
            n - 1
        }
    } else {
        frame
    }
}

impl Anim {
    /// `model_set_anim_frame` (`model.c:2086`).
    pub fn set_frame(&mut self, frame: f32) {
        let Some(anim) = self.animnum else { return };
        let framea = frame.floor() as i32;
        let forwards = self.speed >= 0.0;
        let frameb = if forwards { framea + 1 } else { framea - 1 };
        self.framea = constrain_or_wrap(framea, anim, self.endframe);
        self.frameb = constrain_or_wrap(frameb, anim, self.endframe);
        if self.framea == self.frameb {
            self.frac = 0.0;
            self.frame = self.framea as f32;
        } else if forwards {
            self.frac = frame - framea as f32;
            self.frame = self.framea as f32 + self.frac;
        } else {
            self.frac = 1.0 - (frame - frameb as f32);
            self.frame = self.frameb as f32 + (1.0 - self.frac);
        }
    }

    /// `model_set_anim_frame2` (`model.c:2115`).
    pub fn set_frame2(&mut self, frame1: f32, frame2: f32) {
        self.set_frame(frame1);
        let Some(anim2) = self.animnum2 else { return };
        let framea = frame2.floor() as i32;
        let forwards = self.speed2 >= 0.0;
        let frameb = if forwards { framea + 1 } else { framea - 1 };
        self.frame2a = constrain_or_wrap(framea, anim2, self.endframe2);
        self.frame2b = constrain_or_wrap(frameb, anim2, self.endframe2);
        if self.frame2a == self.frame2b {
            self.frac2 = 0.0;
            self.frame2 = self.frame2a as f32;
        } else if forwards {
            self.frac2 = frame2 - framea as f32;
            self.frame2 = self.frame2a as f32 + self.frac2;
        } else {
            self.frac2 = 1.0 - (frame2 - frameb as f32);
            self.frame2 = self.frame2b as f32 + (1.0 - self.frac2);
        }
    }

    /// `model_copy_anim_for_merge` (`model.c:1733`).
    fn copy_for_merge(&mut self, merge: f32) {
        if merge > 0.0 && self.animnum.is_some() {
            if self.animnum2.is_some() && self.fracmerge == 1.0 {
                return;
            }
            self.frame2 = self.frame;
            self.frac2 = self.frac;
            self.animnum2 = self.animnum;
            self.frame2a = self.framea;
            self.frame2b = self.frameb;
            self.speed2 = self.speed;
            self.newspeed2 = self.newspeed;
            self.oldspeed2 = self.oldspeed;
            self.timespeed2 = self.timespeed;
            self.elapsespeed2 = self.elapsespeed;
            self.endframe2 = self.endframe;
        } else {
            self.animnum2 = None;
        }
    }

    /// `model_set_animation2` (`model.c:1777`), minus the CHRINFO root-motion
    /// bookkeeping (see the module doc).
    fn set_animation2(&mut self, animnum: AnimId, startframe: f32, speed: f32, merge: f32) {
        if self.animnum2.is_some() {
            self.timemerge = merge;
            self.elapsemerge = 0.0;
            self.fracmerge = 1.0;
        } else {
            self.timemerge = 0.0;
            self.fracmerge = 0.0;
        }
        self.animnum = Some(animnum);
        self.endframe = -1.0;
        self.speed = speed;
        self.timespeed = 0.0;
        self.set_frame(startframe);
        self.looping = false;
    }

    /// `model_set_animation` (`model.c:1967`).
    pub fn set_animation(&mut self, animnum: AnimId, startframe: f32, speed: f32, merge: f32) {
        self.copy_for_merge(merge);
        self.set_animation2(animnum, startframe, speed, merge);
    }

    /// `model_set_anim_looping` (`model.c:1988`).
    pub fn set_looping(&mut self, loopframe: f32, loopmerge: f32) {
        self.looping = true;
        self.loopframe = loopframe;
        self.loopmerge = loopmerge;
    }

    /// `model_set_anim_end_frame` (`model.c:1997`).
    pub fn set_end_frame(&mut self, endframe: f32) {
        self.endframe = match self.animnum {
            Some(a) if endframe < anims::num_frames(a) as f32 - 1.0 => endframe,
            _ => -1.0,
        };
    }

    /// `model_set_anim_speed` (`model.c:2026`): tween to `speed` over `startframe`
    /// ticks, or snap when that is 0.
    pub fn set_speed(&mut self, speed: f32, startframe: f32) {
        if startframe > 0.0 {
            self.timespeed = startframe;
            self.newspeed = speed;
            self.elapsespeed = 0.0;
            self.oldspeed = self.speed;
        } else {
            self.speed = speed;
            self.timespeed = 0.0;
        }
    }

    /// `model_get_anim_end_frame` (`model.c:1645`).
    pub fn end_frame(&self) -> f32 {
        if self.endframe >= 0.0 {
            return self.endframe;
        }
        self.animnum.map_or(0.0, |a| anims::num_frames(a) as f32 - 1.0)
    }

    /// `model_is_anim_merging`.
    pub fn is_merging(&self) -> bool {
        self.animnum2.is_some() && self.fracmerge != 0.0 && self.fracmerge != 1.0
    }

    /// `model_tick_anim_quarter_speed` (`model.c:2515`) — the per-frame advance, run
    /// once per rendered frame with that frame's 240 Hz sub-tick count.
    pub fn tick(&mut self, lvupdate240: u32) {
        if self.animnum.is_none() || lvupdate240 == 0 {
            return;
        }
        let mut frame = self.frame;
        let mut frame2 = self.frame2;
        for _ in 0..lvupdate240 {
            // (`timeplay` / playspeed tweening is never used by bots.)
            if self.timemerge > 0.0 {
                self.elapsemerge += self.playspeed * 0.25;
                if self.elapsemerge == 0.0 {
                    self.fracmerge = 1.0;
                } else if self.elapsemerge < self.timemerge {
                    self.fracmerge = (self.timemerge - self.elapsemerge) / self.timemerge;
                } else {
                    self.timemerge = 0.0;
                    self.fracmerge = 0.0;
                    self.animnum2 = None;
                }
            }
            if self.timespeed > 0.0 {
                self.elapsespeed += self.playspeed * 0.25;
                if self.elapsespeed < self.timespeed {
                    self.speed = self.oldspeed + (self.newspeed - self.oldspeed) * self.elapsespeed / self.timespeed;
                } else {
                    self.timespeed = 0.0;
                    self.speed = self.newspeed;
                }
            }
            let speed = self.speed;
            frame += self.playspeed * speed * 0.25;

            if self.animnum2.is_some() {
                if self.timespeed2 > 0.0 {
                    self.elapsespeed2 += self.playspeed * 0.25;
                    if self.elapsespeed2 < self.timespeed2 {
                        self.speed2 = self.oldspeed2
                            + (self.newspeed2 - self.oldspeed2) * self.elapsespeed2 / self.timespeed2;
                    } else {
                        self.timespeed2 = 0.0;
                        self.speed2 = self.newspeed2;
                    }
                }
                frame2 += self.playspeed * self.speed2 * 0.25;
            }

            if self.looping {
                let anim = self.animnum.unwrap();
                let n = anims::num_frames(anim) as f32;
                let realendframe = self.endframe;
                let (startframe, endframe) = if speed >= 0.0 {
                    let mut end = n - 1.0;
                    if realendframe >= 0.0 && end > realendframe {
                        end = realendframe;
                    }
                    (self.loopframe, end)
                } else {
                    let mut start = n - 1.0;
                    if realendframe >= 0.0 && start > realendframe {
                        start = realendframe;
                    }
                    (start, self.loopframe)
                };
                if (speed >= 0.0 && frame >= endframe) || (speed < 0.0 && frame <= endframe) {
                    let (pn, po, pt, pe) = (self.newspeed, self.oldspeed, self.timespeed, self.elapsespeed);
                    self.set_frame2(endframe, 0.0);
                    let (loopmerge, cur_speed) = (self.loopmerge, self.speed);
                    self.set_animation(anim, startframe, cur_speed, loopmerge);
                    self.looping = true;
                    self.endframe = realendframe;
                    self.newspeed = pn;
                    self.oldspeed = po;
                    self.timespeed = pt;
                    self.elapsespeed = pe;
                    frame2 = frame;
                    frame = startframe + frame - endframe;
                }
            }
        }
        if self.animnum2.is_some() {
            self.set_frame2(frame, frame2);
        } else {
            self.set_frame2(frame, 0.0);
        }
    }
}

// ─── Render half ─────────────────────────────────────────────────────────────

/// Every clip in [`anims::ANIMS`], bound to one body's skeleton, in table order.
pub struct ClipBank {
    pub clips: Vec<AnimationClip>,
}

impl ClipBank {
    pub fn load(dir: &str, skeleton: &Skeleton) -> Result<Self, String> {
        let clips = anims::ANIMS
            .iter()
            .map(|a| engine::skeletal::clip::load(&format!("{dir}/{}.glb", a.name), skeleton))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ClipBank { clips })
    }

    /// Local TRS of `anim` at integer frame `frame` (1 frame = 1/30 s of clip time).
    fn frame_trs(&self, anim: AnimId, frame: i32, sk: &Skeleton) -> (Vec<Vec3>, Vec<Quat>, Vec<Vec3>) {
        let n = anims::num_frames(anim) as i32;
        let f = frame.rem_euclid(n.max(1));
        self.clips[anims::index_of(anim)].pose_trs(f as f32 / 30.0, sk)
    }

    /// PD's two-frame tween (`model_tween_rot`, here as a slerp) at `a → b` by `frac`.
    fn tween(&self, anim: AnimId, a: i32, b: i32, frac: f32, sk: &Skeleton) -> (Vec<Vec3>, Vec<Quat>, Vec<Vec3>) {
        let (mut t, mut r, s) = self.frame_trs(anim, a, sk);
        if frac != 0.0 {
            let (t2, r2, _) = self.frame_trs(anim, b, sk);
            for j in 0..r.len() {
                r[j] = r[j].slerp(r2[j], frac);
                t[j] = t[j].lerp(t2[j], frac);
            }
        }
        (t, r, s)
    }
}

/// One joint's extra rotation from `chr_handle_joint_positioned` (radians, PD's
/// axes: x = bend/nod, y = twist, z = roll).
#[derive(Clone, Copy, Debug, Default)]
pub struct JointRot {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// The four joints PD's human callback touches (`g_SkelChr`: neck 0, waist 1,
/// lshoulder 2, rshoulder 3 — matrix slots, which on our rig are `Bone_3`,
/// `Bone_2`, `Bone_4`, `Bone_5`; measured off `a51guard`'s node tree).
#[derive(Clone, Copy, Debug, Default)]
pub struct JointFx {
    pub neck: JointRot,
    pub waist: JointRot,
    pub lshoulder: JointRot,
    pub rshoulder: JointRot,
    /// `chr_get_aimx_angle` — for a bot, its `theta`.
    pub aimangle: f32,
}

/// Joint indices of the callback's four joints on a loaded skeleton.
#[derive(Clone, Copy, Debug)]
pub struct CallbackJoints {
    pub neck: usize,
    pub waist: usize,
    pub lshoulder: usize,
    pub rshoulder: usize,
    pub root: usize,
    pub root_blend: Option<usize>,
    pub right_hand: usize,
    pub left_hand: usize,
}

impl CallbackJoints {
    pub fn resolve(sk: &Skeleton) -> Option<Self> {
        Some(CallbackJoints {
            neck: sk.index_of("Bone_3")?,
            waist: sk.index_of("Bone_2")?,
            lshoulder: sk.index_of("Bone_4")?,
            rshoulder: sk.index_of("Bone_5")?,
            root: sk.index_of("Bone_1")?,
            root_blend: sk.index_of("Blend_1"),
            right_hand: sk.index_of("Bone_9")?,
            left_hand: sk.index_of("Bone_8")?,
        })
    }
}

/// The rotation `chr_handle_joint_positioned` (`chr.c:1832-1880`) premultiplies
/// onto a joint's world matrix, about the joint's own position.
///
/// PD's rotation builders are exactly glam's (checked: `mtx4_load_y_rotation`
/// writes column 0 as `(cos, 0, -sin)`), and `mtx00015be0(a, b)` is `b = a·b`, so
/// the sequence there is `Ry(aim) · Rz(z) · Rx(-x) · Ry(y - aim)`. The `-x` is
/// PD's own negation (`xrot < 0 ? -xrot : 360° - xrot`).
pub fn callback_rotation(r: JointRot, aimangle: f32) -> Quat {
    if r.x != 0.0 || r.z != 0.0 {
        Quat::from_rotation_y(aimangle)
            * Quat::from_rotation_z(r.z)
            * Quat::from_rotation_x(-r.x)
            * Quat::from_rotation_y(r.y - aimangle)
    } else {
        Quat::from_rotation_y(r.y)
    }
}

/// Evaluate a bot's pose: model-space joint globals (in the GLB's own units).
///
/// `model_yaw` is the angle the model is drawn at (`theta - angleoffset` for a
/// bot); it is needed because PD applies the joint callback in *world* space while
/// these globals are in model space. `animscale` is the body's
/// `g_HeadsAndBodies[].animscale`, which PD applies to the clip's root translation
/// only (`model.c:1819`) so a short body's feet still meet the floor.
pub fn evaluate(
    anim: &Anim,
    bank: &ClipBank,
    sk: &Skeleton,
    joints: &CallbackJoints,
    fx: &JointFx,
    model_yaw: f32,
    animscale: f32,
) -> Vec<Mat4> {
    let n = sk.joint_count();
    let Some(a1) = anim.animnum else {
        return sk.global_transforms(&sk.local_bind);
    };
    let (mut t, mut r, s) = bank.tween(a1, anim.framea, anim.frameb, anim.frac, sk);
    if anim.fracmerge != 0.0 {
        if let Some(a2) = anim.animnum2 {
            let (_, r2, _) = bank.tween(a2, anim.frame2a, anim.frame2b, anim.frac2, sk);
            for j in 0..n {
                // `quaternion_slerp(new, old, fracmerge)` — fracmerge 1 is all old.
                r[j] = r[j].slerp(r2[j], anim.fracmerge);
            }
        }
    }
    // The root is drawn at the chr's position; only the clip's vertical travel
    // survives (`chr_update_position` overwrites x/z for bots).
    for j in std::iter::once(joints.root).chain(joints.root_blend) {
        t[j].x = sk.bind_t[j].x;
        t[j].z = sk.bind_t[j].z;
        t[j].y *= animscale;
    }
    let locals: Vec<Mat4> =
        (0..n).map(|i| Mat4::from_scale_rotation_translation(s[i], r[i], t[i])).collect();

    // Hierarchy walk with the callback applied as each joint is positioned, so its
    // children inherit the twist (PD positions children from the modified matrix).
    let to_model = Quat::from_rotation_y(-model_yaw);
    let from_model = Quat::from_rotation_y(model_yaw);
    let extra = |j: usize| -> Option<Quat> {
        let rot = if j == joints.neck {
            fx.neck
        } else if j == joints.waist {
            fx.waist
        } else if j == joints.lshoulder {
            fx.lshoulder
        } else if j == joints.rshoulder {
            fx.rshoulder
        } else {
            return None;
        };
        if rot.x == 0.0 && rot.y == 0.0 && rot.z == 0.0 {
            return None;
        }
        Some(to_model * callback_rotation(rot, fx.aimangle) * from_model)
    };

    let mut global: Vec<Option<Mat4>> = vec![None; n];
    fn resolve(
        i: usize,
        sk: &Skeleton,
        locals: &[Mat4],
        global: &mut Vec<Option<Mat4>>,
        extra: &dyn Fn(usize) -> Option<Quat>,
    ) -> Mat4 {
        if let Some(m) = global[i] {
            return m;
        }
        let mut m = match sk.parents[i] {
            Some(p) => resolve(p, sk, locals, global, extra) * locals[i],
            None => locals[i],
        };
        if let Some(q) = extra(i) {
            let pivot = m.w_axis.truncate();
            m = Mat4::from_translation(pivot) * Mat4::from_quat(q) * Mat4::from_translation(-pivot) * m;
        }
        global[i] = Some(m);
        m
    }
    (0..n).map(|i| resolve(i, sk, &locals, &mut global, &extra)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pd_spike::anims::*;

    #[test]
    fn locomotion_at_half_speed_advances_half_a_frame_per_tick() {
        let mut a = Anim::default();
        a.set_animation(ANIM_0031, 0.0, 0.5, 16.0);
        for _ in 0..10 {
            a.tick(4);
        }
        assert!((a.frame - 5.0).abs() < 1e-4, "{}", a.frame);
    }

    #[test]
    fn a_loop_flagged_clip_wraps_and_a_plain_one_clamps() {
        let mut a = Anim::default();
        a.set_animation(ANIM_0031, 19.5, 1.0, 0.0); // 21 frames, LOOP
        a.tick(8); // +2 frames
        assert!(a.frame < 2.0, "wrapped: {}", a.frame);
        let mut b = Anim::default();
        b.set_animation(ANIM_DEATH_001A, 87.0, 1.0, 0.0); // 89 frames, no LOOP
        b.tick(40);
        assert_eq!(b.frame, 88.0);
    }

    #[test]
    fn negative_speed_plays_backwards_through_the_wrap() {
        let mut a = Anim::default();
        a.set_animation(ANIM_0031, 1.0, -0.5, 0.0);
        for _ in 0..4 {
            a.tick(4); // -2 frames total
        }
        assert!(a.frame > 19.0 && a.frame < 21.0, "{}", a.frame);
    }

    #[test]
    fn changing_animation_merges_over_sixteen_ticks() {
        let mut a = Anim::default();
        a.set_animation(ANIM_006A, 0.0, 0.25, 16.0);
        assert!(a.animnum2.is_none(), "nothing to merge from on the first animation");
        a.set_animation(ANIM_RUNNING_ONEHANDGUN, 0.0, 0.5, 16.0);
        assert_eq!(a.fracmerge, 1.0);
        for _ in 0..8 {
            a.tick(4);
        }
        assert!((a.fracmerge - 0.5).abs() < 1e-4, "{}", a.fracmerge);
        for _ in 0..8 {
            a.tick(4);
        }
        assert!(a.animnum2.is_none());
        assert_eq!(a.fracmerge, 0.0);
    }

    #[test]
    fn an_idle_breathing_loop_stays_inside_its_window() {
        // Heavy-gun idle: ANIM_0002 frames 35-40 at speed 0.05, merge 16.
        let mut a = Anim::default();
        a.set_animation(ANIM_0002, 35.0, 0.05, 16.0);
        a.set_looping(35.0, 16.0);
        a.set_end_frame(40.0);
        for _ in 0..60 * 20 {
            a.tick(4);
            assert!(a.frame >= 35.0 - 1e-3 && a.frame <= 40.0 + 1e-3, "{}", a.frame);
        }
    }

    #[test]
    fn the_callback_rotation_is_pds_matrix_order() {
        // Pure yaw collapses to Ry(y) whatever the aim angle.
        let q = callback_rotation(JointRot { x: 0.0, y: 0.3, z: 0.0 }, 1.1);
        assert!(q.angle_between(Quat::from_rotation_y(0.3)) < 1e-5);
        // Positive x (aim up) tilts a forward vector upwards, about the aim frame.
        let up = callback_rotation(JointRot { x: 0.4, y: 0.0, z: 0.0 }, 0.0) * Vec3::Z;
        assert!(up.y > 0.3, "{up:?}");
        let up_side = callback_rotation(JointRot { x: 0.4, y: 0.0, z: 0.0 }, std::f32::consts::FRAC_PI_2)
            * Vec3::X;
        assert!(up_side.y > 0.3, "aim along +X tilts +X up: {up_side:?}");
    }
}
