//! `bondgun.c`'s projectile makers, run from `hand_tick_attack`:
//! `bgun_create_thrown_projectile` / `bgun_create_thrown_projectile2` /
//! `bgun_configure_projectile` (`:4129`–`:4490`), plus `chr_calculate_trajectory`
//! (`chraction.c:9859`) for the functions that arc onto what you aim at.

use glam::{Mat3, Mat4, Vec3};

use super::gset::*;
use super::pdmtx;
use super::props::{self, Bbox, ObjType, Projectile, WorldObj, PROP_MODEL_SCALE};
use super::range::HitKind;
use super::sim::{Sim, SoundReq};
use crate::pd_spike::pdmath::{baddtor, baddtor2};

/// `chr_calculate_trajectory` (`chraction.c:9859`): the launch direction that
/// lands a throw of speed `arg1` (cm/tick) on `aimpos`, in metres and g.
pub fn chr_calculate_trajectory(frompos: Vec3, arg1: f32, aimpos: Vec3) -> Vec3 {
    let arg1 = arg1 * 0.599_999_99;
    let d = (aimpos - frompos) * 0.01;
    let vel = d.length();
    let latvel = (d.x * d.x + d.z * d.z).sqrt();
    let sp38 = latvel / vel;
    let mut sp40 = sp38.clamp(-1.0, 1.0).acos();
    if d.y < 0.0 {
        sp40 = -sp40;
    }
    let sp2c = ((vel * 9.81 * sp38 * sp38) / (arg1 * arg1) + d.y / vel).clamp(-1.0, 1.0);
    let sp3c = (sp2c.asin() - sp40) * 0.5 + sp40;
    Vec3::new(d.x / latvel * sp3c.cos(), sp3c.sin(), d.z / latvel * sp3c.cos())
}

/// Turn `from` towards `to` by at most `limit` radians — the quaternion slerp
/// of the two look matrices in `bgun_create_thrown_projectile` (`:4370`).
/// Substituted: the great-circle slerp of the two directions (the look
/// matrices share an up vector, so they differ only in the direction).
fn clamp_towards(from: Vec3, to: Vec3, limit: f32) -> Vec3 {
    let radians = from.dot(to).clamp(-1.0, 1.0).acos();
    if radians > limit || radians < -limit {
        let frac = (limit / radians).abs();
        let s = radians.sin();
        if s.abs() < 1e-6 {
            return to;
        }
        (from * ((1.0 - frac) * radians).sin() / s + to * (frac * radians).sin() / s).normalize()
    } else {
        to
    }
}

impl Sim {
    /// `prop_find_aiming_at(HAND_RIGHT, false, FINDPROPCONTEXT_QUERY)` →
    /// `hand->hasdotinfo` / `dotpos`. Substituted: a target board under the
    /// crosshair (the range's only props).
    pub fn aim_dot(&self) -> Option<Vec3> {
        let dir = self.bgun.cam_screen_dir(self.bgun.p.crosspos, 1.0);
        let w = self.bgun.p.projection.transform_vector3(dir);
        match self.range.raycast(self.campos(), w, 65536.0) {
            Some(h) if matches!(h.kind, HitKind::Target(_)) => Some(h.pos),
            _ => None,
        }
    }

    pub fn alloc_obj_id(&mut self) -> u32 {
        self.next_obj_id += 1;
        self.next_obj_id
    }

    /// `bgun_create_thrown_projectile` (`bondgun.c:4294`).
    pub fn bgun_create_thrown_projectile(&mut self, h: usize, weaponnum: i32, weaponfunc: usize) {
        let hand = &self.bgun.hands[h];
        let muzzlepos = hand.muzzlepos;
        let mut sp1f4 = Mat4::IDENTITY;
        if weaponnum == WEAPON_COMBATKNIFE {
            sp1f4 = pdmtx::load_z_rotation(baddtor(270.0));
            let sp190 = pdmtx::load_x_rotation(baddtor(180.0));
            sp1f4 = pdmtx::mul(&sp190, &sp1f4);
        }
        // The muzzle matrix's rotation (camera space, as PD uses it).
        let mm = hand.muzzlemat;
        let sp190 = Mat4::from_cols(
            mm.x_axis.truncate().normalize_or_zero().extend(0.0),
            mm.y_axis.truncate().normalize_or_zero().extend(0.0),
            mm.z_axis.truncate().normalize_or_zero().extend(0.0),
            glam::Vec4::W,
        );
        sp1f4 = pdmtx::mul(&sp190, &sp1f4);

        // Spawn at the muzzle unless a wall is between it and the player.
        let playerpos = self.player.pos;
        let to = muzzlepos - playerpos;
        let blocked = to.length() > 0.0 && self.range.raycast(playerpos, to.normalize(), to.length()).is_some();
        let spawnpos = if blocked { playerpos } else { muzzlepos };

        let gundir = self.bgun.bgun_calculate_player_shot_spread(h, true);
        let mut gundir = self.bgun.p.projection.transform_vector3(gundir);
        let calc = self.gset.func(weaponnum, weaponfunc).is_some_and(|f| f.flags & FUNCFLAG_CALCULATETRAJECTORY != 0);
        let mut velocity = if calc {
            if let Some(aimpos) = self.aim_dot() {
                let sp140 = chr_calculate_trajectory(spawnpos, 21.666_666, aimpos);
                gundir = clamp_towards(gundir, sp140, baddtor2(20.0));
            }
            gundir * 21.666_666
        } else {
            let mut v = gundir * 16.666_666;
            if weaponnum == WEAPON_GRENADE || weaponnum == WEAPON_NBOMB {
                v.y += 1.666_666_6;
            } else {
                v.y += 5.0;
            }
            v
        };
        if weaponnum == WEAPON_LAPTOPGUN {
            self.bgun.bgun_free_weapon_pub(h);
        }
        let lv = self.lv();
        if lv.lvupdate240 > 0 {
            velocity += (self.player.pos - self.prev_player_pos) / lv.lvupdate60freal;
        }
        let Some(id) = self.bgun_create_thrown_projectile2(weaponnum, weaponfunc, spawnpos, &sp1f4, velocity) else { return };
        let primetimer60 = self.bgun.hands[h].primetimer60;
        let Some(o) = self.objs.iter_mut().find(|o| o.id == id) else { return };
        if o.ty == ObjType::Weapon && weaponnum == WEAPON_GRENADE && weaponfunc == FUNC_PRIMARY {
            // Cooked in the hand: the fuse already burned `primetimer60`.
            if o.timer240 < primetimer60 * 4 {
                o.timer240 = 0;
            } else {
                o.timer240 -= primetimer60 * 4;
            }
            o.gunfunc = weaponfunc;
        }
        if let Some(p) = o.proj.as_mut() {
            p.flags |= props::PROJECTILEFLAG_LAUNCHING;
            p.nextsteppos = muzzlepos;
            if weaponnum == WEAPON_GRENADE && weaponfunc == FUNC_SECONDARY {
                p.hitspeedpreservationfrac = 1.0;
            }
            if weaponnum == WEAPON_COMBATKNIFE {
                p.flags |= props::PROJECTILEFLAG_FORCEGOODBOUNCE;
                p.hitspeedpreservationfrac = 0.1;
                p.pickuptimer240 = 240;
                o.thrownknife = true;
            }
        }
    }

    /// `bgun_create_thrown_projectile2` (`bondgun.c:4199`).
    pub fn bgun_create_thrown_projectile2(&mut self, weaponnum: i32, weaponfunc: usize, pos: Vec3, arg4: &Mat4, velocity: Vec3) -> Option<u32> {
        let func = self.gset.func(weaponnum, weaponfunc)?.clone();
        let proj = func.proj.clone()?;
        let spin = if weaponnum == WEAPON_COMBATKNIFE {
            // guRotateF(90 / (RANDOMFRAC() + 12.1) degrees, about arg4's y axis).
            let deg = 90.0 / (self.bgun.rng.randomfrac() + 12.1);
            let axis = arg4.y_axis.truncate().normalize_or_zero();
            Mat3::from_axis_angle(axis, deg.to_radians())
        } else {
            props::projectile_load_random_rotation(&mut self.bgun.rng)
        };
        let stem = props::projectile_model_stem(proj.projectilemodelnum)?;
        let def = self.models.get(stem)?.clone();
        let id = self.alloc_obj_id();
        let bbox = Bbox::from_def(&def);
        let mut o = WorldObj {
            id,
            ty: ObjType::Weapon,
            weaponnum,
            gunfunc: weaponfunc,
            timer240: -1,
            def,
            bbox,
            scale: PROP_MODEL_SCALE,
            pos,
            realrot: Mat3::IDENTITY,
            proj: None,
            attached: false,
            embedded_board: None,
            thrownknife: false,
            deleting: false,
            dangerous: false,
            settlerot_byactualsize: false,
            settlerot_laptop: false,
            throwthrough: false,
            heldrocket: false,
            owner: 0,
            autogun: None,
        };
        if weaponnum == WEAPON_LAPTOPGUN {
            // laptop_deploy (`propobj.c:17488`): an autogun, one per player —
            // a second deploy blows up the first.
            if let Some(old) = self.objs.iter_mut().find(|o| o.ty == ObjType::Autogun && o.owner == 0 && !o.deleting) {
                old.deleting = true;
                let at = old.pos;
                self.explosion_create_simple(at, super::explosions::EXPLOSIONTYPE_LAPTOP);
            }
            o.ty = ObjType::Autogun;
            o.settlerot_laptop = true;
            o.autogun = Some(self.laptop_deploy_state());
        } else {
            // Note this timer is converted to 240 time immediately below.
            o.timer240 = func.activatetime60;
            if o.timer240 >= 2 {
                o.timer240 *= 4;
            }
            if weaponnum == WEAPON_GRENADE || weaponnum == WEAPON_NBOMB {
                o.dangerous = true;
            }
            if matches!(proj.projectilemodelnum, 0x113 | 0x114 | 0x115) {
                o.settlerot_byactualsize = true;
            }
        }
        self.bgun_configure_projectile(&mut o, pos, arg4, velocity, spin);
        if let Some(p) = o.proj.as_mut() {
            p.flags |= props::PROJECTILEFLAG_FORCEGOODBOUNCE;
            p.hitspeedpreservationfrac = 0.1;
            p.pickuptimer240 = 240;
        }
        let (pan, volume) = (self.pan_of(pos), self.ps_vol(0x80a9, pos));
        self.sounds.push(SoundReq { id: 0x80a9, speed: 1.0, pan, volume, loop_hand: None });
        self.objs.push(o);
        Some(id)
    }

    /// `bgun_configure_projectile` (`bondgun.c:4129`): place it (orientation ×
    /// model scale), make it an airborne, sticky projectile with its spin.
    pub fn bgun_configure_projectile(&mut self, o: &mut WorldObj, pos: Vec3, matrix1: &Mat4, velocity: Vec3, spin: Mat3) {
        let mut m = *matrix1;
        pdmtx::scale3(&mut m, o.scale);
        o.realrot = Mat3::from_mat4(m);
        o.pos = pos;
        let lvframenum = self.lvframenum;
        o.proj = Some(Projectile {
            flags: props::PROJECTILEFLAG_AIRBORNE | props::PROJECTILEFLAG_STICKY,
            has_owner: true,
            mtx: spin,
            speed: velocity,
            startframe: lvframenum,
            ..Projectile::default()
        });
    }

    /// `player_activate_remote_mine_detonator` (`propobj.c:17211`).
    pub fn player_activate_remote_mine_detonator(&mut self) {
        self.detonating_mines = true;
        self.sounds.push(SoundReq { id: 0x80ab, speed: 1.0, pan: 0.0, volume: 1.0, loop_hand: None });
        self.bgun.bgun_start_detonate_animation();
    }
}

impl Sim {
    /// `bgun_create_held_rocket` (`bondgun.c:4527`): the rocket the launcher
    /// shows loaded — a WEAPON_ROCKET object flagged HELDROCKET/THROWTHROUGH.
    pub fn bgun_create_held_rocket(&mut self, h: usize) {
        if self.bgun.hands[h].rocket.is_some() {
            return;
        }
        self.bgun.hands[h].firedrocket = false;
        let Some(proj) = self.gset.func(WEAPON_ROCKETLAUNCHER, self.bgun.hands[h].weaponfunc).and_then(|f| f.proj.clone()) else { return };
        let Some(stem) = props::projectile_model_stem(proj.projectilemodelnum) else { return };
        let Some(def) = self.models.get(stem).cloned() else { return };
        let id = self.alloc_obj_id();
        let bbox = Bbox::from_def(&def);
        let pos = self.bgun.hands[h].muzzlepos;
        self.objs.push(WorldObj {
            id,
            ty: ObjType::Weapon,
            weaponnum: props::WEAPON_ROCKET,
            gunfunc: 0,
            timer240: 1,
            def,
            bbox,
            scale: PROP_MODEL_SCALE,
            pos,
            realrot: Mat3::from_diagonal(Vec3::splat(PROP_MODEL_SCALE)),
            proj: None,
            attached: false,
            embedded_board: None,
            thrownknife: false,
            deleting: false,
            dangerous: false,
            settlerot_byactualsize: false,
            settlerot_laptop: false,
            throwthrough: true,
            heldrocket: true,
            owner: 0,
            autogun: None,
        });
        self.bgun.hands[h].rocket = Some(id);
    }

    /// `bgun_update_held_rocket` (`bondgun.c:4491`): while unfired it sits at
    /// the muzzle in the hand's orientation (drawn with the gun, from
    /// `muzzlemat`).
    pub fn bgun_update_held_rocket(&mut self, h: usize) {
        let Some(id) = self.bgun.hands[h].rocket else { return };
        let hand = &self.bgun.hands[h];
        let (fired, posmtx, muzzlepos) = (hand.firedrocket, hand.posmtx, hand.muzzlepos);
        if let Some(o) = self.objs.iter_mut().find(|o| o.id == id) {
            if !fired {
                let mut m = posmtx;
                m.w_axis = glam::Vec4::W;
                pdmtx::scale3(&mut m, o.scale);
                o.realrot = Mat3::from_mat4(m);
                o.pos = muzzlepos;
            }
        }
    }

    /// `bgun_update_rocket_launcher` (`bondgun.c:6997`).
    pub fn bgun_update_rocket_launcher(&mut self, h: usize) {
        if self.bgun.hands[h].rocket.is_none() && self.bgun.hands[h].loadedammo[0] > 0 {
            self.bgun_create_held_rocket(h);
        }
        if self.bgun.hands[h].rocket.is_some() {
            self.bgun_update_held_rocket(h);
        }
    }

    /// `bgun_free_held_rocket` (`bondgun.c:4552`).
    pub fn bgun_free_held_rocket(&mut self, h: usize) {
        if let Some(id) = self.bgun.hands[h].rocket.take() {
            self.objs.retain(|o| o.id != id);
        }
    }

    /// The held rockets to draw in the gun pass: (model, camera-space joints)
    /// rooted at the hand's `muzzlemat` (`bgun_update_held_rocket`).
    pub fn held_rockets(&self) -> Vec<(String, Vec<Mat4>)> {
        let mut out = Vec::new();
        for h in 0..2 {
            let hand = &self.bgun.hands[h];
            let Some(id) = hand.rocket else { continue };
            if !hand.visible {
                continue;
            }
            if let Some(o) = self.objs.iter().find(|o| o.id == id && o.heldrocket) {
                // matrices[0] = muzzlemat; model_update_relations_quick leaves
                // the rest (bgun_update_held_rocket).
                let mut mats = vec![Mat4::IDENTITY; o.def.nummatrices.max(1)];
                mats[0] = hand.muzzlemat;
                out.push((o.def.name.clone(), mats));
            }
        }
        out
    }

    /// `bgun_create_fired_projectile` (`bondgun.c:4562`): rockets, Slayer
    /// rockets, crossbow bolts, Devastator and SuperDragon grenade rounds.
    pub fn bgun_create_fired_projectile(&mut self, h: usize) {
        let weaponnum = self.bgun.hands[h].weaponnum;
        let weaponfunc = self.bgun.hands[h].weaponfunc;
        let Some(func) = self.gset.func(weaponnum, weaponfunc).cloned() else { return };
        if func.ftype != INVENTORYFUNCTYPE_SHOOT_PROJECTILE {
            return;
        }
        let Some(proj) = func.proj.clone() else { return };
        let sp270 = Mat3::IDENTITY;
        let gundir = self.bgun.bgun_calculate_player_shot_spread(h, true);
        let mut gundir = self.bgun.p.projection.transform_vector3(gundir);
        let mut spawnpos = self.bgun.hands[h].muzzlepos;
        if weaponnum == WEAPON_SLAYER && weaponfunc == FUNC_SECONDARY {
            spawnpos += gundir * 50.0;
        }
        let sp260 = proj.speed * 1.666_666_6 / 60.0;
        let sp25c = proj.traveldist * 1.666_666_6;
        if func.flags & FUNCFLAG_CALCULATETRAJECTORY != 0 {
            if let Some(aimpos) = self.aim_dot() {
                let sp1bc = chr_calculate_trajectory(spawnpos, sp25c, aimpos);
                gundir = clamp_towards(gundir, sp1bc, baddtor2(10.0));
            }
        }
        let accel = gundir * sp260;
        let lv = self.lv();
        let mut sp264 = accel * lv.lvupdate60freal + gundir * sp25c;
        if func.flags & FUNCFLAG_FLYBYWIRE == 0 && lv.lvupdate240 > 0 {
            sp264 += (self.player.pos - self.prev_player_pos) / lv.lvupdate60freal;
        }
        let mut sp210 = self.bgun.hands[h].posmtx;
        sp210.w_axis = glam::Vec4::W;

        // The object: the launcher's own rocket, or a new one.
        let (weapon_id, objweapon, gunfunc) = if let Some(id) = self.bgun.hands[h].rocket {
            self.bgun.hands[h].firedrocket = true;
            self.bgun.hands[h].rocket = None;
            let wn = if func.flags & FUNCFLAG_HOMINGROCKET != 0 { props::WEAPON_HOMINGROCKET } else { props::WEAPON_ROCKET };
            (Some(id), wn, 0)
        } else {
            let (wn, gf) = match weaponnum {
                WEAPON_ROCKETLAUNCHER | WEAPON_SLAYER => {
                    (if func.flags & FUNCFLAG_HOMINGROCKET != 0 { props::WEAPON_HOMINGROCKET } else { props::WEAPON_ROCKET }, 0)
                }
                WEAPON_CROSSBOW => (props::WEAPON_BOLT, weaponfunc),
                WEAPON_DEVASTATOR => (props::WEAPON_GRENADEROUND, weaponfunc),
                WEAPON_SUPERDRAGON => (props::WEAPON_GRENADEROUND, props::FUNC_2),
                _ => (weaponnum, weaponfunc),
            };
            (None, wn, gf)
        };
        let id = match weapon_id {
            Some(id) => id,
            None => {
                let Some(stem) = props::projectile_model_stem(proj.projectilemodelnum) else { return };
                let Some(def) = self.models.get(stem).cloned() else { return };
                let id = self.alloc_obj_id();
                let bbox = Bbox::from_def(&def);
                self.objs.push(WorldObj {
                    id,
                    ty: ObjType::Weapon,
                    weaponnum: objweapon,
                    gunfunc,
                    timer240: -1,
                    def,
                    bbox,
                    scale: PROP_MODEL_SCALE,
                    pos: spawnpos,
                    realrot: Mat3::IDENTITY,
                    proj: None,
                    attached: false,
                    embedded_board: None,
                    thrownknife: false,
                    deleting: false,
                    dangerous: false,
                    settlerot_byactualsize: false,
                    settlerot_laptop: false,
                    throwthrough: false,
                    heldrocket: false,
                    owner: 0,
                    autogun: None,
                });
                id
            }
        };
        let playerpos = self.player.pos;
        let lvframenum = self.lvframenum;
        let Some(idx) = self.objs.iter().position(|o| o.id == id) else { return };
        let mut o = self.objs.remove(idx);
        o.weaponnum = objweapon;
        o.throwthrough = false;
        o.heldrocket = false;
        o.timer240 = proj.timer60;
        if o.timer240 != -1 {
            o.timer240 *= 4;
        }
        // bgun_create_fired_projectile2: placed at the player, stepped to the muzzle.
        self.bgun_configure_projectile(&mut o, playerpos, &sp210, sp264, sp270);
        let p = o.proj.as_mut().unwrap();
        p.flags |= props::PROJECTILEFLAG_LAUNCHING;
        p.nextsteppos = spawnpos;
        if func.flags & FUNCFLAG_PROJECTILE_LIGHTWEIGHT != 0 {
            p.flags |= props::PROJECTILEFLAG_LIGHTWEIGHT;
        } else if func.flags & FUNCFLAG_PROJECTILE_POWERED != 0 {
            p.flags |= props::PROJECTILEFLAG_POWERED;
        }
        p.powerlimit240 = 1200;
        p.accel = accel;
        p.pickuptimer240 = 240;
        p.hitspeedpreservationfrac = proj.hitspeedpreservationfrac;
        p.speeddecel = proj.speeddecel * 1.666_666_6;
        p.startframe = lvframenum;
        if proj.scale != 1.0 {
            o.scale *= proj.scale;
            o.realrot *= proj.scale;
        }
        if func.soundnum > 0 {
            let (pan, volume) = (self.pan_of(spawnpos), self.ps_vol(func.soundnum, spawnpos));
            self.sounds.push(SoundReq { id: func.soundnum, speed: 1.0, pan, volume, loop_hand: None });
        }
        if func.flags & FUNCFLAG_FLYBYWIRE != 0 {
            self.player_launch_slayer_rocket(id);
        }
        // projectile_launch right away (PROJECTILEFLAG_LAUNCHING).
        {
            let mut out = props::ObjOut::default();
            let campos = self.campos();
            let mut c = props::ObjCtx {
                rng: &mut self.bgun.rng,
                lv,
                gset: &self.gset,
                world: &self.range,
                smokes: &mut self.smokes,
                explosions: &mut self.explosions,
                campos,
                lodscalez: self.bgun.p.c_lodscalez,
                playerpos,
                detonating: false,
                brightness: 0.0,
                out: &mut out,
            };
            let (mut a, mut b) = (Vec3::ZERO, Vec3::ZERO);
            props::projectile_launch(&mut o, &mut c, &mut a, &mut b);
        }
        self.objs.push(o);
    }

    /// `player_launch_slayer_rocket` (`player.c:3034`): the camera rides the
    /// rocket and the player steers it.
    pub fn player_launch_slayer_rocket(&mut self, id: u32) {
        self.slayer = Some(super::sim::SlayerCam { rocket: id, badrockettime: 0 });
        self.visionmode = super::sim::VisionMode::SlayerRocket;
    }
}
