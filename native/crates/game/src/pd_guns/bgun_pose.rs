//! `bondgun.c`, the half that places the gun on screen every frame: the sway
//! blend, the crosshair swivel, shot spread, gangsta tilt, the per-weapon model
//! tweaks (slide, Reaper barrel, sniper scope, Devastator loader, shotgun star),
//! the muzzle flash, casings/beams, and `bgun0f0a5550` — the function that turns
//! all of it into the gun model's camera-space matrices.

use glam::{Mat4, Vec3, Vec4};

use super::anim::AnimCtx;
use super::bgun::*;
use super::gset::*;
use super::pdmtx;
use crate::pd_spike::pdmath::{baddtor, dtor};

/// `func0f096b70` (`game_096b20.c:15`): Catmull-Rom through four points.
fn catmull(a: Vec3, b: Vec3, c: Vec3, d: Vec3, t: f32) -> Vec3 {
    let sq = t * t;
    let cu = sq * t;
    let m0 = sq - 0.5 * (t + cu);
    let m1 = 1.5 * cu - 2.5 * sq + 1.0;
    let m2 = -1.5 * cu + 2.0 * sq + 0.5 * t;
    let m3 = 0.5 * (cu - sq);
    a * m0 + b * m1 + c * m2 + d * m3
}

/// A drawable snapshot of one hand, for the renderer.
pub struct HandDraw {
    pub hand: usize,
    /// Camera-space joint matrices (PD `hand->gunmodel.matrices`), cm.
    pub matrices: Vec<Mat4>,
    pub mirror: bool,
    pub brighter: bool,
}

impl Bgun {
    /// `bgun_calculate_blend` (`:3228`): pick the next random sway key.
    pub(crate) fn bgun_calculate_blend(&mut self, h: usize) {
        let sway = self.gset.weapon(self.bgun_get_weapon_num(h)).map_or(1.0, |w| w.sway);
        let sp60 = ((self.hands[h].curblendpos + 2) % 4) as usize;
        let sp58 = (self.hands[h].curblendpos + 1) % 4;
        self.hands[h].curblendpos = sp58;
        let r: [f32; 8] = std::array::from_fn(|_| self.rng.randomfrac());
        let hand = &mut self.hands[h];
        hand.blendlook[sp60] = Vec3::new((r[0] - 0.5) * 0.08 * sway, (r[1] - 0.5) * 0.1 * sway, -1.0);
        hand.blendup[sp60] = Vec3::new((r[2] - 0.5) * 0.1 * sway, 1.0, (r[3] - 0.5) * 0.1 * sway);
        hand.blendpos[sp60] = Vec3::new(r[4] * 0.75 + 1.5, (2.0 + r[5]) * hand.blendscale1, (r[6] - 0.5) * 2.5);
        if hand.sideflag < 0 {
            hand.blendpos[sp60].x *= -1.0;
            hand.sideflag = if hand.sideflag == -2 { 1 } else { -2 };
        } else {
            hand.sideflag = if hand.sideflag == 2 { -1 } else { 2 };
        }
        hand.blendscale1 = -hand.blendscale1;
    }

    /// `bgun_update_blend` (`:3271`): damped spline sway → damppos/look/up.
    pub(crate) fn bgun_update_blend(&mut self, h: usize) {
        let xshift = self.hands[h].xshift;
        let amp = self.p.gunposamplitude;
        let lv240 = self.lv.lvupdate240;
        let hand = &mut self.hands[h];
        let pos = hand.curblendpos as usize;
        let i0 = (pos + 3) % 4;
        let i2 = (pos + 1) % 4;
        let i3 = (pos + 2) % 4;
        let t = hand.dampt;
        let mut sp5c = catmull(hand.blendpos[i0], hand.blendpos[pos], hand.blendpos[i2], hand.blendpos[i3], t);
        let sp50 = catmull(hand.blendlook[i0], hand.blendlook[pos], hand.blendlook[i2], hand.blendlook[i3], t);
        let sp44 = catmull(hand.blendup[i0], hand.blendup[pos], hand.blendup[i2], hand.blendup[i3], t);
        sp5c *= amp;
        sp5c.x += hand.adjustdamp.x;
        sp5c.y += hand.adjustdamp.y;
        sp5c.x += xshift;
        for _ in 0..lv240 {
            hand.damppossum = hand.damppossum * 0.9872 + sp5c;
            hand.damplooksum = hand.damplooksum * 0.9872 + sp50;
            hand.dampupsum = hand.dampupsum * 0.9872 + sp44;
        }
        hand.damppos = hand.damppossum * 0.012_799_978 * 2.0;
        hand.damplook = hand.damplooksum * 0.012_799_978;
        hand.dampup = hand.dampupsum * 0.012_799_978;
    }

    /// `bgun0f09d8dc` (`:3343`): drive the sway from movement.
    /// `breathing` is `bhead_get_breathing_value`, `arg1` the gun speed (heart
    /// rate), `arg2` vertical look + crouch speed, `arg3` turn speed, `arg4` strafe.
    pub fn bgun_update_sway(&mut self, breathing: f32, arg1: f32, arg2: f32, arg3: f32, arg4: f32) {
        let lv240 = self.lv.lvupdate240;
        let lv60 = self.lv.lvupdate60freal;
        let sp50 = arg2.abs();
        let p = &mut self.p;
        if arg1 > 0.8 {
            p.gunposamplitude = 1.0;
        } else if arg1 > 0.1 {
            let tmp = 1.0 - ((arg1 - 0.1) * baddtor(360.0) / 2.8).cos();
            p.gunposamplitude = 0.8 * tmp + 0.2;
        } else {
            p.gunposamplitude = 0.1;
        }
        if p.crouchpos != CROUCHPOS_SQUAT && p.gunposamplitude < 0.3 * p.bondbreathing {
            p.gunposamplitude = 0.3 * p.bondbreathing;
        }
        if p.gunposamplitude < 0.5 * sp50 {
            p.gunposamplitude = 0.5 * sp50;
        }
        for _ in 0..lv240 {
            p.gunampsum = 0.9872 * p.gunampsum + p.gunposamplitude;
        }
        p.gunposamplitude = 0.012_799_978 * p.gunampsum;
        let mut breathing = breathing;
        if breathing < (1.0 / 60.0) * sp50 {
            breathing = (1.0 / 60.0) * sp50;
        }
        for _ in 0..lv240 {
            p.cyclesum = 0.9872 * p.cyclesum + breathing;
        }
        let breathing = p.cyclesum * 0.012_799_978;
        let sp4c = breathing * lv60;
        let mut dampt0 = self.hands[0].dampt + sp4c;
        while dampt0 >= 1.0 {
            self.bgun_calculate_blend(HAND_RIGHT);
            dampt0 -= 1.0;
            self.p.syncoffset += 1;
        }
        self.p.synccount += lv60;
        if self.p.synccount > 60.0 {
            self.p.synccount = 0.0;
            self.p.syncchange = (self.rng.randomfrac() - 0.5) * 0.2 / 60.0;
        }
        if self.p.syncchange + sp4c > 0.0 {
            self.p.gunsync += self.p.syncchange;
        }
        let p = &mut self.p;
        if p.gunsync > 0.5 {
            p.gunsync = 0.5;
        } else if p.gunsync < -0.5 {
            p.gunsync = -0.5;
        } else if p.gunsync < 0.1 && p.gunsync > -0.1 {
            p.gunsync = if p.gunsync > 0.0 { -0.1 } else { 0.1 };
        }
        let mut dampt1 = dampt0 + self.p.syncoffset as f32 + self.p.gunsync;
        while dampt1 >= 1.0 {
            self.bgun_calculate_blend(HAND_LEFT);
            dampt1 -= 1.0;
            self.p.syncoffset -= 1;
        }
        let dampts = [dampt0, dampt1];
        for (i, hand) in self.hands.iter_mut().enumerate() {
            hand.dampt = dampts[i];
            hand.adjustdamp.x = -1.75 * arg3 + -0.8 * arg4;
            hand.adjustdamp.y = -2.0 * arg2;
        }
    }

    // ─── camera helpers (camera.c) ───────────────────────────────────────────

    /// `cam0f0b4c3c` (`camera.c:108`): a screen position → camera-space direction.
    pub fn cam_screen_dir(&self, pos2d: [f32; 2], len: f32) -> Vec3 {
        let p = &self.p;
        let halfw = p.screen_width * 0.5;
        let halfh = p.screen_height * 0.5;
        let sp1c = (halfh - (pos2d[1] - p.screen_top)) * p.c_scaley;
        let sp20 = (pos2d[0] - p.screen_left - halfw) * p.c_scalex;
        let sp18 = -1.0;
        let f2 = len / (sp20 * sp20 + sp1c * sp1c + sp18 * sp18).sqrt();
        Vec3::new(sp20 * f2, sp1c * f2, sp18 * f2)
    }

    /// `cam_set_scale` (`camera.c:69`) — call when the viewport or fov changes.
    pub fn cam_set_scale(&mut self) {
        let p = &mut self.p;
        let halfh = p.screen_height * 0.5;
        let halfw = p.screen_width * 0.5;
        let a = p.fovy * (dtor(180.0) / 360.0);
        p.c_scaley = a.sin() / (a.cos() * halfh);
        p.c_scalex = (p.c_scaley * p.aspect * halfh) / halfw;
        // c_lodscalez: this view's scale against a 60-degree, 240-line one.
        let lod60 = dtor(30.0).sin() / (dtor(30.0).cos() * 120.0);
        p.c_lodscalez = p.c_scaley / lod60;
    }

    // ─── swivel (4866-5140) ──────────────────────────────────────────────────

    /// `bgun_swivel` (`:4866`), with `hasdotinfo` skipped (multiplayer).
    pub fn bgun_swivel(&mut self, screenx: f32, screeny: f32, crossdamp: f32, aimdamp: f32) {
        let sw = self.p.screen_width;
        let shh = self.p.screen_height;
        let mut x = [screenx, screenx];
        let mut y = [screeny, screeny];
        let _ignore = [!self.hands[HAND_LEFT].inuse, !self.hands[HAND_RIGHT].inuse];

        // Right hand only + reloading: recentre until the reload is nearly done.
        if !self.hands[HAND_LEFT].inuse && self.hands[HAND_RIGHT].state == HANDSTATE_RELOAD && self.hands[HAND_RIGHT].animcmd.is_some() {
            let numframes = if self.hands[HAND_RIGHT].weaponnum == WEAPON_CROSSBOW { 5 } else { 25 };
            let n = self.hands[HAND_RIGHT].anim.num_frames(&self.bank);
            if (self.bgun_get_current_keyframe(HAND_RIGHT) as i32) < n - numframes {
                x[HAND_RIGHT] = 0.0;
                y[HAND_RIGHT] = 0.0;
            }
        }
        if self.hands[HAND_RIGHT].weaponnum == WEAPON_UNARMED {
            x[HAND_RIGHT] = self.swivel_extra[0];
            y[HAND_RIGHT] = self.swivel_extra[1];
        }

        let p = &mut self.p;
        p.oldcrosspos = p.crosspos;
        if crossdamp != p.guncrossdamp {
            p.crosspossum[0] = p.crosspossum[0] * (1.0 - p.guncrossdamp) / (1.0 - crossdamp);
            p.crosspossum[1] = p.crosspossum[1] * (1.0 - p.guncrossdamp) / (1.0 - crossdamp);
            p.guncrossdamp = crossdamp;
        }
        if aimdamp != p.gunaimdamp {
            p.crosssum2[0] = p.crosssum2[0] * (1.0 - p.gunaimdamp) / (1.0 - aimdamp);
            p.crosssum2[1] = p.crosssum2[1] * (1.0 - p.gunaimdamp) / (1.0 - aimdamp);
            p.gunaimdamp = aimdamp;
        }
        for _ in 0..self.lv.lvupdate240 {
            p.crosspossum[0] = p.crosspossum[0] * crossdamp + screenx;
            p.crosspossum[1] = p.crosspossum[1] * crossdamp + screeny;
            for (hi, hand) in self.hands.iter_mut().enumerate() {
                hand.guncrosspossum[0] = 0.926_969_7 * hand.guncrosspossum[0] + x[hi];
                hand.guncrosspossum[1] = 0.926_969_7 * hand.guncrosspossum[1] + y[hi];
            }
        }
        let p = &mut self.p;
        p.crosspos[0] = (p.crosspossum[0] * (1.0 - crossdamp) * sw * 0.5 + sw * 0.5).clamp(3.0, sw - 4.0) + p.screen_left;
        p.crosspos[1] = (p.crosspossum[1] * (1.0 - crossdamp) * shh * 0.5 + shh * 0.5).clamp(3.0, shh - 4.0) + p.screen_top;
        let (left, top) = (p.screen_left, p.screen_top);
        for hand in self.hands.iter_mut() {
            hand.crosspos[0] = (hand.guncrosspossum[0] * 0.073_030_29 * sw * 0.5 + sw * 0.5).clamp(3.0, sw - 4.0) + left;
            hand.crosspos[1] = (hand.guncrosspossum[1] * 0.073_030_29 * shh * 0.5 + shh * 0.5).clamp(3.0, shh - 4.0) + top;
        }
        let p = &mut self.p;
        for _ in 0..self.lv.lvupdate240 {
            p.crosssum2[0] = p.crosssum2[0] * aimdamp + screenx;
            p.crosssum2[1] = p.crosssum2[1] * aimdamp + screeny;
        }
        p.crosspos2[0] = p.crosssum2[0] * (1.0 - aimdamp) * sw * 0.5 + sw * 0.5 + p.screen_left;
        p.crosspos2[1] = p.crosssum2[1] * (1.0 - aimdamp) * shh * 0.5 + shh * 0.5 + p.screen_top;
        let aimpos = self.cam_screen_dir(self.p.crosspos2, 1000.0);
        self.bgun_set_aim_pos(aimpos);
    }

    /// `bgun_swivel_with_damp` (`:5034`).
    pub fn bgun_swivel_with_damp(&mut self, screenx: f32, screeny: f32, crossdamp: f32) {
        let w = self.gset.weapon(self.bgun_get_weapon_num(HAND_RIGHT)).map(|w| w.aim.aimdamp).unwrap_or(0.9767);
        let aimdamp = w.max(crossdamp);
        self.bgun_swivel(screenx, screeny, crossdamp, aimdamp);
    }

    /// `bgun_swivel_without_damp` (`:5052`).
    pub fn bgun_swivel_without_damp(&mut self, screenx: f32, screeny: f32) {
        let aimdamp = self.gset.weapon(self.bgun_get_weapon_num(HAND_RIGHT)).map(|w| w.aim.aimdamp).unwrap_or(0.9767);
        self.bgun_swivel(screenx, screeny, 0.945, aimdamp);
    }

    /// `bgun_set_aim_pos` (`:9246`).
    fn bgun_set_aim_pos(&mut self, coord: Vec3) {
        for h in 0..2 {
            let xs = self.hands[h].xshift;
            self.hands[h].aimpos = Vec3::new(xs + coord.x, coord.y, coord.z);
        }
    }

    /// `bgun_calculate_player_shot_spread` (`:5086`): the camera-space direction a
    /// round leaves in (from the eye, through the crosshair plus spread).
    pub fn bgun_calculate_player_shot_spread(&mut self, h: usize, dorandom: bool) -> Vec3 {
        let mut spread = 0.0;
        if let Some(s) = self.func_of(h).and_then(|f| f.shoot) {
            spread = s.spread;
        }
        if self.gset.has_aim_flag(self.bgun_get_weapon_num(h), INVAIMFLAG_ACCURATESINGLESHOT) && self.hands[h].burstbullets == 1 {
            spread *= 0.25;
        }
        if self.p.crouchpos == CROUCHPOS_SQUAT {
            spread *= 0.5;
        }
        if self.hands[HAND_LEFT].inuse {
            spread *= 1.5;
        }
        let scaledspread = 120.0 * spread / self.p.fovy;
        // vi_get_height(): the framebuffer height. The spike's screen space is
        // PD's (screen_height), so spread stays in PD pixels at any window size.
        let vi_height = self.p.screen_height;
        let mut rf = || if dorandom { (self.rng.randomfrac() - 0.5) * self.rng.randomfrac() } else { 0.0 };
        let rx = rf();
        let ry = rf();
        let cx = self.p.crosspos[0] + rx * scaledspread * self.p.screen_width / (vi_height * self.p.aspect);
        let cy = self.p.crosspos[1] + (ry * scaledspread * self.p.screen_height) / vi_height;
        self.cam_screen_dir([cx, cy], 1.0)
    }

    // ─── per-weapon model tweaks (6421-7036) ─────────────────────────────────

    /// `bgun_update_gangsta` (`:6421`). Returns the z-roll matrix to premultiply.
    fn bgun_update_gangsta(&mut self, h: usize, pos: &mut Vec3) -> Mat4 {
        let func = self.func_of(h);
        let lv240 = self.lv.lvupdate240;
        let lv60 = self.lv.lvupdate60freal;
        let gangsta = self.ctrl.gangsta;
        let hand = &mut self.hands[h];
        let state_ok = matches!(hand.state, HANDSTATE_IDLE | HANDSTATE_2 | HANDSTATE_ATTACKEMPTY | HANDSTATE_ATTACK);
        let shoots = func.as_ref().is_some_and(|f| f.kind() == INVENTORYFUNCTYPE_SHOOT);
        if gangsta && shoots && state_ok {
            if hand.gangstarot < 1.0 {
                hand.ispare1 += lv240;
                if hand.ispare1 > 60 {
                    hand.gangstarot += lv60 / 30.0;
                    if hand.gangstarot > 1.0 {
                        hand.gangstarot = 1.0;
                    }
                }
            } else {
                hand.ispare1 = 0;
            }
        } else {
            let inversespeed = if hand.animmode == HANDANIMMODE_BUSY { 15.0 } else { 30.0 };
            if hand.gangstarot > 0.0 {
                let mut revert = false;
                hand.ispare1 += lv240;
                if hand.gangstarot < 1.0 {
                    hand.ispare1 = 244;
                }
                if hand.ispare1 > 120 {
                    revert = true;
                }
                if hand.animmode == HANDANIMMODE_BUSY && func.as_ref().is_some_and(|f| f.kind() != INVENTORYFUNCTYPE_SHOOT) {
                    revert = true;
                }
                if !state_ok {
                    revert = true;
                }
                if revert {
                    hand.gangstarot -= lv60 / inversespeed;
                }
                if hand.gangstarot < 0.0 {
                    hand.gangstarot = 0.0;
                }
            } else {
                hand.ispare1 = 0;
            }
        }
        let tmp = -(hand.gangstarot * dtor(180.0)).cos() * 0.5 + 0.5;
        let side = if h != HAND_RIGHT { 1.0 } else { -1.0 };
        let z = (tmp * 66.6 * 0.017_453_292) * side;
        pos.y += 4.0 * hand.gangstarot;
        pos.x += 2.0 * hand.gangstarot * side;
        pdmtx::load_rotation(Vec3::new(0.0, 0.0, z))
    }

    /// `bgun_update_reaper` (`:6740`): barrel spin state + the joint callback angle.
    fn bgun_update_reaper(&mut self, h: usize) {
        let lv60 = self.lv.lvupdate60freal;
        let hand = &mut self.hands[h];
        // mm_reaperspeedaim = matmot2, mm_reaperspeedcur = matmot3, mm_reaperrot = matmot1
        if hand.matmot3 <= hand.matmot2 {
            if hand.matmot2 < 0.0 {
                hand.matmot2 += 0.01 * lv60;
                if hand.matmot2 > 0.0 {
                    hand.matmot2 = 0.0;
                }
            }
            hand.matmot3 = hand.matmot2;
        } else {
            let mut f12 = lv60 * (1.0 / 200.0);
            if hand.matmot2 < 0.000_000_1 {
                hand.matmot2 = -0.14;
                if hand.matmot3 < 0.15 {
                    f12 *= 4.0;
                }
            }
            let mut f2 = hand.matmot3 - hand.matmot2;
            if f12 < f2 {
                f2 = f12;
            }
            hand.matmot3 -= f2;
        }
        let spin = (1.0 - (hand.matmot3 * dtor(180.0)).cos()) * 0.5 * lv60 * 0.2;
        if hand.matmot3 < 0.0 {
            hand.matmot1 -= spin;
        } else {
            hand.matmot1 += spin;
        }
        let tmp = (hand.matmot1 / (3.141_59 * 2.0)) as i32;
        hand.matmot1 -= tmp as f32 * (3.141_59 * 2.0);
        self.reaper_rot = hand.matmot1;
        if !hand.audiohandle && hand.matmot3 > 0.1 && self.lv.lvupdate240 != 0 {
            hand.audiohandle = true;
            self.events.push(GunEvent::Sound { id: 0x805e, speed: 1.0 });
        }
        if hand.audiohandle && hand.matmot3 < 0.1 {
            hand.audiohandle = false;
            self.events.push(GunEvent::StopLoop { hand: h });
        }
    }

    /// Node matrix index of a part on the hand's gun model.
    fn part_mtx(&self, h: usize, partnum: i32) -> Option<usize> {
        let m = self.hands[h].gunmodel.as_ref()?;
        let node = m.def.get_part(partnum)?;
        m.def.find_node_mtx_index(node)
    }

    /// `bgun0f0a4e44` (`:7142`): orient + scale the muzzle flash this tick.
    fn bgun_orient_flash(&mut self, h: usize, maxburst: usize, muzzle_slot: usize, arg9: &Mat4, func: Option<&FuncDef>) {
        let weaponnum = self.hands[h].weaponnum;
        let muzzlez = self.gset.weapon(weaponnum).map_or(1.0, |w| w.muzzlez);
        let mut index = (self.hands[h].burstbullets as usize) % maxburst.max(1);
        let mut shotstotake = self.hands[h].shotstotake;
        let spb4 = self.rng.randomfrac() * 0.25 + 1.0;
        if func.is_some_and(|f| f.flags & FUNCFLAG_00000001 != 0) {
            let _ = self.rng.randomfrac(); // the overwritten random roll
        }
        let mut spd8 = pdmtx::load_z_rotation((self.rng.randomfrac() as f64 * 0.3 - 0.15) as f32);
        let aimpos = self.hands[h].aimpos;
        let Some(model) = self.hands[h].gunmodel.as_mut() else { return };
        spd8 = model.matrices[muzzle_slot] * spd8;
        pdmtx::scale3(&mut spd8, spb4);
        pdmtx::scale_col2(&mut spd8, muzzlez);
        model.matrices[muzzle_slot] = spd8;
        if shotstotake == 0 && weaponnum != WEAPON_REAPER {
            shotstotake += 1;
        }
        let mut on = [false; 3];
        for _ in 0..shotstotake {
            on[index] = true;
            index += 1;
            if index >= maxburst {
                index = 0;
            }
        }
        let toggles = self.hands[h].flash_toggles.clone();
        for (i, &node) in toggles.iter().enumerate().take(maxburst) {
            if on[i] {
                if let Some(m) = self.hands[h].gunmodel.as_mut() {
                    m.visible[node] = true;
                }
            }
        }
        if weaponnum == WEAPON_REAPER || weaponnum == WEAPON_SHOTGUN {
            return;
        }
        for partnum in 0x50..=0x52 {
            let (node, rodata_pos, slot) = {
                let Some(m) = self.hands[h].gunmodel.as_ref() else { return };
                let Some(node) = m.def.get_part(partnum) else { continue };
                let super::model::NodeKind::Position { pos, mtx, .. } = m.def.nodes[node].kind else { continue };
                (node, pos, mtx[0] as usize)
            };
            let _ = node;
            let sp60 = spd8.transform_point3(rodata_pos);
            let roll = self.rng.randomfrac() * baddtor(360.0);
            let mut sp70 = pdmtx::mtx4_align(roll, -sp60.x, -sp60.y, -sp60.z);
            pdmtx::scale3(&mut sp70, 0.100_000_01 * spb4);
            let m = self.hands[h].gunmodel.as_mut().unwrap();
            let root = m.matrices[0].w_axis.truncate();
            let d = root - aimpos;
            let arg10 = pdmtx::mtx00016e98(0.0, d.x, d.y, d.z);
            sp70 = arg10 * sp70;
            pdmtx::scale_row2(&mut sp70, muzzlez);
            sp70 = *arg9 * sp70;
            pdmtx::set_translation(&mut sp70, sp60);
            m.matrices[slot] = sp70;
        }
    }

    /// `bgun_create_fx` (`:7234`): casing + beam for a fired weapon.
    fn bgun_create_fx(&mut self, h: usize, weaponnum: i32) {
        self.ctrl.throwing = false;
        let func = self.func_of(h);
        if let Some(f) = &func {
            if weaponnum != WEAPON_DY357MAGNUM && weaponnum != WEAPON_DY357LX && self.hands[h].gunmodel.is_some() {
                let partnum = if weaponnum == WEAPON_REAPER {
                    if self.hands[h].burstbullets & 1 == 1 {
                        MODELPART_REAPER_CARTEJECTPOS1
                    } else {
                        MODELPART_REAPER_CARTEJECTPOS2
                    }
                } else {
                    MODELPART_GUN_CARTEJECTPOS
                };
                let casing = self
                    .gset
                    .weapon(weaponnum)
                    .and_then(|w| if f.ammoindex >= 0 { w.ammos[f.ammoindex as usize].as_ref() } else { None })
                    .map_or(-1, |a| a.casingeject);
                let mtx = match self.part_mtx(h, partnum) {
                    Some(slot) => {
                        let mut m = self.hands[h].gunmodel.as_ref().unwrap().matrices[slot];
                        pdmtx::scale3(&mut m, 9.999_999);
                        self.p.projection * m
                    }
                    None => self.hands[h].posmtx,
                };
                if casing >= 0 && f.kind() == INVENTORYFUNCTYPE_SHOOT {
                    self.events.push(GunEvent::Casing { hand: h, mtx, casing });
                }
                self.bgun_set_part_visible(h, MODELPART_GUN_CARTFLAPCLOSED, false);
                self.bgun_set_part_visible(h, MODELPART_GUN_CARTFLAPOPEN, true);
            }
            if f.ftype == INVENTORYFUNCTYPE_SHOOT_PROJECTILE || f.kind() == INVENTORYFUNCTYPE_THROW {
                self.events.push(GunEvent::UncloakTemporarily);
            }
        }
        let createbeam = match &func {
            Some(f) => {
                !(f.kind() == INVENTORYFUNCTYPE_MELEE
                    || f.ftype & INVENTORYFUNCTYPE_0200 != 0
                    || f.kind() == INVENTORYFUNCTYPE_SPECIAL
                    || f.kind() == INVENTORYFUNCTYPE_THROW)
            }
            None => true,
        };
        if createbeam
            && matches!(
                weaponnum,
                WEAPON_FALCON2
                    | WEAPON_FALCON2_SILENCER
                    | WEAPON_FALCON2_SCOPE
                    | WEAPON_MAGSEC4
                    | WEAPON_MAULER
                    | WEAPON_PHOENIX
                    | WEAPON_DY357MAGNUM
                    | WEAPON_DY357LX
                    | WEAPON_CMP150
                    | WEAPON_CYCLONE
                    | WEAPON_CALLISTO
                    | WEAPON_RCP120
                    | WEAPON_LAPTOPGUN
                    | WEAPON_DRAGON
                    | WEAPON_K7AVENGER
                    | WEAPON_AR34
                    | WEAPON_SUPERDRAGON
                    | WEAPON_REAPER
                    | WEAPON_SNIPERRIFLE
                    | WEAPON_FARSIGHT
                    | WEAPON_TRANQUILIZER
                    | WEAPON_LASER
            )
        {
            self.events.push(GunEvent::Beam { hand: h });
            self.hands[h].numfires += 1;
        }
    }

    /// `bgun_update_smoke` (`:6517`).
    fn bgun_update_smoke(&mut self, h: usize, weaponnum: i32) {
        let func = self.func_of(h);
        let lv60 = self.lv.lvupdate60freal;
        let dual = self.hands[HAND_LEFT].inuse;
        let hand = &mut self.hands[h];
        if hand.firing {
            if weaponnum == WEAPON_DY357MAGNUM || weaponnum == WEAPON_DY357LX {
                if func.as_ref().is_some_and(|f| f.kind() == INVENTORYFUNCTYPE_SHOOT) {
                    hand.gunsmokepoint += 0.6;
                }
            } else {
                hand.gunsmokepoint += 0.2;
            }
        }
        hand.gunsmokepoint -= lv60 / 120.0;
        if hand.gunsmokepoint < 0.0 {
            hand.gunsmokepoint = 0.0;
        }
        if func.as_ref().is_some_and(|f| f.kind() == INVENTORYFUNCTYPE_SHOOT) {
            let mult = if dual { 1.5 } else { 1.0 };
            hand.forcecreatesmoke = false;
            match weaponnum {
                WEAPON_FALCON2 | WEAPON_FALCON2_SCOPE => {
                    if hand.gunsmokepoint * mult > 0.66 {
                        hand.createsmoke = true;
                    }
                }
                WEAPON_MAGSEC4 | WEAPON_MAULER => {
                    if hand.gunsmokepoint * mult > 0.75 {
                        hand.createsmoke = true;
                    }
                }
                WEAPON_DY357MAGNUM | WEAPON_DY357LX => {
                    if hand.gunsmokepoint * mult > 0.9 {
                        hand.createsmoke = true;
                    }
                }
                WEAPON_CMP150 | WEAPON_DRAGON | WEAPON_K7AVENGER | WEAPON_AR34 | WEAPON_SUPERDRAGON => {
                    hand.forcecreatesmoke = true;
                    if hand.burstbullets > 14 {
                        hand.createsmoke = true;
                    }
                }
                WEAPON_CYCLONE | WEAPON_LAPTOPGUN => {
                    if hand.burstbullets > 20 {
                        hand.createsmoke = true;
                    }
                    hand.forcecreatesmoke = true;
                }
                WEAPON_RCP120 => {
                    hand.forcecreatesmoke = true;
                    if hand.burstbullets > 25 {
                        hand.createsmoke = true;
                    }
                }
                WEAPON_REAPER | WEAPON_SHOTGUN => {
                    if weaponnum == WEAPON_REAPER {
                        hand.forcecreatesmoke = true;
                    }
                    if hand.firing {
                        hand.createsmoke = true;
                    }
                }
                _ => {}
            }
        }
        if hand.createsmoke && (hand.state != HANDSTATE_ATTACK || hand.forcecreatesmoke) {
            let kind = match weaponnum {
                WEAPON_FALCON2 | WEAPON_FALCON2_SCOPE | WEAPON_MAGSEC4 | WEAPON_MAULER | WEAPON_DY357MAGNUM | WEAPON_DY357LX => 1,
                WEAPON_REAPER => 2,
                WEAPON_SHOTGUN => 3,
                _ => 0,
            };
            // createsmoke is cleared by the world once smoke_create_for_hand
            // succeeds (`bondgun.c:6631`).
            hand.gunsmokepoint = 0.0;
            let pos = hand.muzzlepos;
            self.events.push(GunEvent::Smoke { hand: h, pos, kind });
        }
    }

    // ─── bgun0f0a5550: the per-hand pose (7336-7885) ─────────────────────────

    /// `bgun0f0a5550` (`:7336`): position the hand's gun for this frame, tick its
    /// animation, apply the model tweaks, find the muzzle, and queue fx.
    fn bgun_pose_hand(&mut self, h: usize) {
        let weaponnum = self.bgun_get_weapon_num(h);
        let Some(weapondef) = self.gset.weapon(weaponnum).cloned() else {
            self.hands[h].visible = false;
            return;
        };
        let func = self.func_of(h);
        let shoot = func.as_ref().and_then(|f| f.shoot.clone());
        let lv60 = self.lv.lvupdate60freal;

        self.bgun_update_blend(h);

        let other_has_40 = self.gset.has_flag(self.bgun_get_weapon_num(1 - h), WEAPONFLAG_00000040);
        {
            let hand = &mut self.hands[h];
            if h == HAND_RIGHT {
                if other_has_40 {
                    hand.xshift = (hand.xshift + 2.0 * lv60 / 240.0).min(2.0);
                } else {
                    hand.xshift = (hand.xshift - 2.0 * lv60 / 240.0).max(0.0);
                }
            } else if other_has_40 {
                hand.xshift = (hand.xshift - 2.0 * lv60 / 240.0).max(-2.0);
            } else {
                hand.xshift = (hand.xshift + 2.0 * lv60 / 240.0).min(0.0);
            }
        }

        let xpos = self.gset_get_xpos(h);
        let hand = &self.hands[h];
        let mut sp274 = if h == HAND_RIGHT {
            Vec3::new(xpos + hand.damppos.x + hand.adjustpos.x, weapondef.posy + hand.damppos.y + hand.adjustpos.y, weapondef.posz + hand.damppos.z + hand.adjustpos.z)
        } else {
            Vec3::new(xpos + hand.damppos.x - hand.adjustpos.x, weapondef.posy + hand.damppos.y + hand.adjustpos.y, weapondef.posz + hand.damppos.z + hand.adjustpos.z)
        };
        sp274.y += self.p.guncloseroffset * 5.0 / -90.0 * 50.0;
        sp274.z -= self.p.guncloseroffset * 15.0 / -90.0 * 50.0;

        if self.hands[h].firing && self.lv.lvupdate240 != 0 {
            if let Some(r) = shoot.as_ref().and_then(|s| s.recoil) {
                let fm = self.hands[h].finalmult[0];
                sp274.x += (self.rng.randomfrac() - 0.5) * r.xrange * fm;
                sp274.y += (self.rng.randomfrac() - 0.5) * r.yrange * fm;
                sp274.z += (self.rng.randomfrac() - 0.5) * r.zrange * fm;
            }
        }

        // The gun follows the aim point (crosspos2) by guntrans side/up/down.
        let p = &self.p;
        let aim = &weapondef.aim;
        let fspare1 = (p.crosspos2[0] - p.screen_left - p.screen_width * 0.5) * aim.guntransside / (p.screen_width * 0.5);
        let dy = p.crosspos2[1] - p.screen_top - p.screen_height * 0.5;
        let fspare2 = if dy > 0.0 {
            dy * aim.guntransdown / (p.screen_height * 0.5)
        } else {
            dy * aim.guntransup / (p.screen_height * 0.5)
        };
        self.hands[h].fspare1 = fspare1;
        self.hands[h].fspare2 = fspare2;
        sp274.x += fspare1;
        sp274.y -= fspare2;

        let mode = self.hands[h].mode;
        let visible = self.gset.has_flag(weaponnum, WEAPONFLAG_00000040)
            && !self.gset.has_flag(weaponnum, WEAPONFLAG_00000080)
            && mode != HANDMODE_6
            && mode != HANDMODE_7
            && self.bgun_is_loaded()
            && self.hands[h].inuse
            && self.ctrl.gunmemtype != 0
            && self.hands[h].gunmodel.is_some();
        self.hands[h].visible = visible;

        if visible {
            // bgun_execute_model_cmd_list: every toggle back to visible.
            if let Some(m) = self.hands[h].gunmodel.as_mut() {
                m.reset_toggles();
            }
            if let Some(m) = self.hands[h].handmodel.as_mut() {
                m.reset_toggles();
            }
            self.bgun_update_ammo_visibility(h);
            if self.gset.has_flag(weaponnum, WEAPONFLAG_HASGUNSCRIPT) {
                self.bgun_tick_anim(h);
            }
        }

        let mut sp234 = Mat4::IDENTITY;
        if self.gset.has_flag(weaponnum, WEAPONFLAG_GANGSTA) {
            let roll = self.bgun_update_gangsta(h, &mut sp274);
            sp234 = roll * sp234;
        }
        if self.hands[h].useposrot {
            let pr = self.hands[h].posrotmtx;
            sp274 += pr.w_axis.truncate();
            sp234 = pdmtx::mul(&pr, &sp234);
            sp234.w_axis = Vec4::new(0.0, 0.0, 0.0, 1.0);
        } else {
            let hand = &mut self.hands[h];
            hand.rotxoffset = 0.0;
            hand.posoffset = Vec3::ZERO;
        }
        let (dl, du) = (self.hands[h].damplook, self.hands[h].dampup);
        let sp284 = pdmtx::look_at_basis(Vec3::ZERO, dl, du);
        sp234 = pdmtx::mul(&sp284, &sp234);

        let sp164 = pdmtx::load_rotation(Vec3::new(0.0, dtor(180.0), 0.0));
        let sp118 = self.cam_screen_dir(self.hands[h].crosspos, 1.0) * 1000.0;
        let ang = |a0: f32, a1: f32, a2: f32, a3: f32| -> f32 {
            let a = a0 - a2;
            (a / (a * a + (a1 - a3) * (a1 - a3)).sqrt()).asin()
        };
        let sp1a4 = Vec3::new(ang(sp118.y, sp118.z, sp274.y, sp274.z), -ang(sp118.x, sp118.z, sp274.x, sp274.z), 0.0);
        self.hands[h].lastrotangx = sp1a4.x;
        self.hands[h].lastrotangy = sp1a4.y;
        let sp124 = pdmtx::load_rotation(sp1a4);
        let sp284 = sp124 * sp164;
        sp234 = sp284 * sp234;
        let mut rendermtx = sp234;
        pdmtx::set_translation(&mut rendermtx, sp274);

        self.hands[h].cammtx = rendermtx;
        self.hands[h].prevmtx = self.hands[h].posmtx;
        self.hands[h].posmtx = pdmtx::mul(&self.p.projection, &rendermtx);

        if visible {
            // Flash toggles 0x5a..0x5c, hidden unless this tick fires.
            let mut flash_toggles = Vec::new();
            if let Some(m) = self.hands[h].gunmodel.as_ref() {
                for j in 0x5a..0x5d {
                    if let Some(node) = m.def.get_part(j) {
                        flash_toggles.push(node);
                    }
                }
            }
            self.hands[h].flash_toggles = flash_toggles.clone();

            self.hands[h].dualflip = self.gset.has_flag(weaponnum, WEAPONFLAG_DUALFLIP) && h == HAND_LEFT;
            if self.hands[h].dualflip {
                pdmtx::scale_col0_xyz(&mut rendermtx, -1.0);
            }
            pdmtx::scale3(&mut rendermtx, 0.100_000_01);

            if weaponnum == WEAPON_REAPER {
                self.bgun_update_reaper(h);
            }

            // model_set_matrices_with_anim(&renderdata, &hand->gunmodel)
            let bank = self.bank.clone();
            let reaper = weaponnum == WEAPON_REAPER;
            let (spin_slot, cyl_slots) = if reaper {
                (
                    self.part_mtx(h, MODELPART_REAPER_002C),
                    [self.part_mtx(h, MODELPART_REAPER_002D), self.part_mtx(h, MODELPART_REAPER_002E), self.part_mtx(h, MODELPART_REAPER_002F)],
                )
            } else {
                (None, [None; 3])
            };
            let rot = self.reaper_rot;
            let hand = &mut self.hands[h];
            let anim = hand.anim.clone();
            if let Some(model) = hand.gunmodel.as_mut() {
                // bgun0f0a256c: the Reaper's spinning barrels.
                let mut cb = |slot: usize, m: &mut Mat4| {
                    if Some(slot) == spin_slot {
                        *m = *m * pdmtx::load_rotation(Vec3::new(0.0, 0.0, rot));
                    }
                    if cyl_slots.contains(&Some(slot)) {
                        *m = *m * pdmtx::load_rotation(Vec3::new(0.0, 0.0, 2.0 * -rot));
                    }
                };
                let jf: Option<super::model::JointFn> = if reaper { Some(&mut cb) } else { None };
                model.set_matrices_with_anim(&rendermtx, Some(&anim), &bank, jf);
            }

            // The slide (MODELPART_GUN_SLIDE) slides back along its own -z.
            if let Some(slot) = self.part_mtx(h, MODELPART_GUN_SLIDE) {
                self.bgun_update_slide(h);
                let t = self.hands[h].slidetrans;
                let m = &mut self.hands[h].gunmodel.as_mut().unwrap().matrices[slot];
                let v = pdmtx::rotate(m, Vec3::new(0.0, 0.0, -t));
                m.w_axis += v.extend(0.0);
            }

            for &node in &flash_toggles {
                if let Some(m) = self.hands[h].gunmodel.as_mut() {
                    m.visible[node] = false;
                }
            }
            self.hands[h].star_flash = false;

            match weaponnum {
                WEAPON_SNIPERRIFLE => self.bgun_update_sniper_rifle(h),
                WEAPON_DEVASTATOR => self.bgun_update_devastator(h),
                WEAPON_SHOTGUN => self.bgun_update_shotgun(h),
                _ => {}
            }

            let mut muzzle = self.part_mtx(h, MODELPART_GUN_MUZZLEPOS);
            if weaponnum == WEAPON_REAPER {
                let k = if self.hands[h].flashon || self.hands[h].firing {
                    self.hands[h].burstbullets % 3
                } else {
                    self.lv.lvframenum % 3
                };
                muzzle = self.part_mtx(h, MODELPART_REAPER_001E + k);
            }
            if let Some(slot) = muzzle {
                let m = self.hands[h].gunmodel.as_ref().unwrap().matrices[slot];
                self.hands[h].muzzlemat = m;
                self.hands[h].muzzlepos = pdmtx::transform(&self.p.projection, m.w_axis.truncate());
                self.hands[h].muzzlez = -m.w_axis.z;
                if self.hands[h].flashon && !flash_toggles.is_empty() && weaponnum != WEAPON_SHOTGUN && self.lv.lvupdate240 != 0 {
                    let f = self.func_of(h);
                    self.bgun_orient_flash(h, flash_toggles.len(), slot, &sp234, f.as_ref());
                }
            } else if let Some(slot) = self.part_mtx(h, MODELPART_GUN_HOLDPOS) {
                let m = self.hands[h].gunmodel.as_ref().unwrap().matrices[slot];
                self.hands[h].muzzlemat = m;
                self.hands[h].muzzlepos = pdmtx::transform(&self.p.projection, m.w_axis.truncate());
                self.hands[h].muzzlez = -m.w_axis.z;
            } else {
                let pm = self.hands[h].posmtx;
                self.hands[h].muzzlepos = pm.w_axis.truncate();
                self.hands[h].muzzlemat = pm;
                self.hands[h].muzzlez = -self.hands[h].cammtx.w_axis.z;
            }
        } else {
            let pm = self.hands[h].posmtx;
            self.hands[h].muzzlepos = pm.w_axis.truncate();
            self.hands[h].muzzlemat = pm;
            self.hands[h].muzzlez = -self.hands[h].cammtx.w_axis.z;
        }

        if weaponnum == WEAPON_ROCKETLAUNCHER {
            self.events.push(GunEvent::UpdateRocketLauncher { hand: h });
        }

        if self.hands[h].firing && self.lv.lvupdate240 != 0 {
            self.bgun_create_fx(h, weaponnum);
        }
        if self.lv.lvupdate240 != 0 {
            self.bgun_update_smoke(h, weaponnum);
        }
        self.hands[h].animframeinc = 0;
    }

    /// `bgun_update_sniper_rifle` (`:6842`): telescope the scope with the zoom.
    fn bgun_update_sniper_rifle(&mut self, h: usize) {
        let f26 = 1.0 - (self.gset_get_gun_zoom_fov() - 2.0) / 58.0;
        for i in 0..4 {
            let Some(slot) = self.part_mtx(h, MODELPART_SNIPERRIFLE_SCOPE1 + i) else { continue };
            let f20 = f26 * 4.0;
            let mut v = f20 - i as f32;
            if f20 < i as f32 {
                v = 0.0;
            }
            v *= 100.0;
            let m = &mut self.hands[h].gunmodel.as_mut().unwrap().matrices[slot];
            let d = pdmtx::rotate(m, Vec3::new(0.0, 0.0, v));
            m.w_axis += d.extend(0.0);
        }
    }

    /// `bgun_update_devastator` (`:6886`).
    fn bgun_update_devastator(&mut self, h: usize) {
        let Some(slot) = self.part_mtx(h, MODELPART_DEVASTATOR_0028) else { return };
        let lv60 = self.lv.lvupdate60freal;
        let hand = &mut self.hands[h];
        hand.loadslide = (hand.loadslide + 0.01 * lv60).min(1.0);
        let x = hand.loadslide * -10.0 * 1.636;
        let m = &mut hand.gunmodel.as_mut().unwrap().matrices[slot];
        let d = pdmtx::rotate(m, Vec3::new(x, 0.0, 0.0));
        m.w_axis += d.extend(0.0);
    }

    /// `bgun_update_shotgun` (`:6919`): the starburst on the blast.
    fn bgun_update_shotgun(&mut self, h: usize) {
        let lv60 = self.lv.lvupdate60freal;
        let slot = self.part_mtx(h, MODELPART_SHOTGUN_0050);
        let hand = &mut self.hands[h];
        if hand.flashon {
            hand.matmot1 = 1.0;
        }
        if hand.matmot1 > 0.0 {
            hand.matmot1 -= lv60 / 6.0;
            if hand.matmot1 < 0.01 {
                hand.matmot1 = 0.0;
            }
        }
        if hand.matmot1 > 0.0 {
            if let Some(&node) = hand.flash_toggles.first() {
                if let Some(m) = hand.gunmodel.as_mut() {
                    m.visible[node] = true;
                }
            }
            if let Some(slot) = slot {
                let f = hand.matmot1;
                let m = &mut hand.gunmodel.as_mut().unwrap().matrices[slot];
                pdmtx::scale_col2(m, (1.0 - f) * 8.0 + 0.5);
                pdmtx::scale_col0(m, (1.0 - f) * 3.0 + 1.0);
                pdmtx::scale_col1(m, (1.0 - f) * 3.0 + 1.0);
            }
        }
    }

    fn gunzoomfov_index(&self) -> Option<usize> {
        match self.bgun_get_weapon_num(HAND_RIGHT) {
            WEAPON_SNIPERRIFLE => Some(0),
            WEAPON_FARSIGHT => Some(1),
            _ => None,
        }
    }

    /// `gset_zoom_out` (`gset.c:215`): widen by `1 + amount·0.1` per frame, the
    /// Farsight at half rate, up to 60°.
    pub fn gset_zoom_out(&mut self, fovpersec: f32, lv60: f32) {
        let Some(i) = self.gunzoomfov_index() else { return };
        let mut amount = fovpersec * 0.25 * lv60;
        if self.bgun_get_weapon_num(HAND_RIGHT) == WEAPON_FARSIGHT {
            amount *= 0.5;
        }
        self.p.gunzoomfovs[i] = (self.p.gunzoomfovs[i] * (1.0 + amount * 0.1)).min(60.0);
    }

    /// `gset_zoom_in` (`gset.c:250`): the same, narrowing, down to 2°.
    pub fn gset_zoom_in(&mut self, fovpersec: f32, lv60: f32) {
        let Some(i) = self.gunzoomfov_index() else { return };
        let mut amount = fovpersec * 0.25 * lv60;
        if self.bgun_get_weapon_num(HAND_RIGHT) == WEAPON_FARSIGHT {
            amount *= 0.5;
        }
        self.p.gunzoomfovs[i] = (self.p.gunzoomfovs[i] / (1.0 + amount * 0.1)).max(2.0);
    }

    /// `gset_get_gun_zoom_fov` (`gset.c:187`).
    pub fn gset_get_gun_zoom_fov(&self) -> f32 {
        match self.bgun_get_weapon_num(HAND_RIGHT) {
            WEAPON_SNIPERRIFLE => self.p.gunzoomfovs[0],
            WEAPON_FARSIGHT => self.p.gunzoomfovs[1],
            w => self.gset.weapon(w).map_or(0.0, |w| w.aim.zoomfov),
        }
    }

    /// `bgun_tick_gameplay2` (`:7964`): load ticking + both hands' poses.
    pub fn bgun_tick_gameplay2(&mut self) {
        self.bgun_tick_load();
        if self.ctrl.weaponnum == WEAPON_MAULER {
            self.bgun_tick_mauler_charge();
        }
        for i in 0..2 {
            for s in self.hands[i].gunroundsspent.iter_mut() {
                *s = s.saturating_sub(self.lv.lvupdate60 as u16);
            }
        }
        self.bgun_pose_hand(HAND_RIGHT);
        if self.hands[HAND_LEFT].inuse {
            self.bgun_pose_hand(HAND_LEFT);
        } else {
            self.hands[HAND_LEFT].ejectstate = EJECTSTATE_INACTIVE;
            self.hands[HAND_LEFT].visible = false;
        }
    }

    /// The hands to draw this frame.
    pub fn draws(&self) -> Vec<HandDraw> {
        let mut out = Vec::new();
        for h in 0..2 {
            let hand = &self.hands[h];
            if !hand.visible {
                continue;
            }
            if let Some(m) = &hand.gunmodel {
                out.push(HandDraw {
                    hand: h,
                    matrices: m.matrices.clone(),
                    mirror: hand.dualflip,
                    brighter: self.gset.has_flag(hand.weaponnum, WEAPONFLAG_BRIGHTER),
                });
            }
        }
        out
    }

    /// `bgun_anim` helper for tests: advance a hand's animation outside the pose.
    pub fn anim_ctx<'a>(bank: &'a super::animdata::AnimBank) -> AnimCtx<'a> {
        AnimCtx { bank, scale: 1.0, chrinfo: None, merging_enabled: true }
    }
}
