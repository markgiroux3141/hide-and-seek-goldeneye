//! `bondgun.c`, second half of the hand state machine: recoil, the attack paths,
//! empty clicks, weapon switching (with the gun-memory load latency), function
//! toggling, and `bgun_tick_gameplay`.

use super::bgun::*;
use super::gset::*;
use super::model::Model;
use super::pdmtx;
use crate::pd_spike::pdmath::{baddtor, dtor};

impl Bgun {
    /// `bgun_tick_recoil` (`:1851`). Returns true when the gun may fire again.
    pub(crate) fn bgun_tick_recoil(&mut self, h: usize, shoot: &ShootDef) -> bool {
        let unk24 = shoot.unk24;
        let unk25 = shoot.unk25;
        let mut sum = unk24 + unk25;
        let unk26 = shoot.unk26;
        let unk27 = shoot.unk27;
        let recoverytime60 = shoot.recoverytime60;
        let weaponnum = self.hands[h].weaponnum;
        let (posz, xpos) = {
            let w = self.gset.weapon(weaponnum);
            (w.map_or(0.0, |w| w.posz), self.gset_get_xpos(h))
        };
        let curframe = self.hands[h].stateframes - self.hands[h].statevar1;

        if sum <= 0 {
            sum = 0;
        } else {
            let hand = &mut self.hands[h];
            if hand.triggerreleased
                && hand.triggeron
                && curframe >= unk26
                && unk26 > 0
                && unk27 >= 0
                && hand.stateflags & HANDSTATEFLAG_00000040 == 0
                && curframe + unk27 < sum
            {
                // Fired during recoil
                hand.stateflags |= HANDSTATEFLAG_00000040;
                hand.statevar1 = curframe;
                hand.rotxstart = hand.rotxoffset;
                hand.rotxend = 0.0;
                hand.posend = glam::Vec3::ZERO;
                hand.posstart = hand.posoffset;
            }
            if hand.stateflags & HANDSTATEFLAG_00000040 != 0 {
                if curframe - hand.statevar1 < unk27 {
                    let mult1 = ((unk27 - curframe + hand.statevar1) as f32 * dtor(90.0) / unk27 as f32).cos() * 0.5 + 0.5;
                    hand.rotxoffset = pdmtx::tween_rot_axis(hand.rotxstart, hand.rotxend, mult1);
                    hand.useposrot = true;
                    hand.posoffset = (hand.posend - hand.posstart) * mult1 + hand.posstart;
                    hand.posrotmtx = pdmtx::load_x_rotation(hand.rotxoffset);
                    pdmtx::set_translation(&mut hand.posrotmtx, hand.posoffset);
                } else {
                    hand.posrotmtx = glam::Mat4::IDENTITY;
                    hand.useposrot = false;
                    return true;
                }
            }
            if curframe < sum && hand.stateflags & HANDSTATEFLAG_00000040 == 0 {
                let recoildist = shoot.recoildist;
                let recoilangle = shoot.recoilangle;
                if hand.stateflags & HANDSTATEFLAG_00000080 == 0 {
                    hand.stateflags |= HANDSTATEFLAG_00000080;
                    hand.rotxstart = hand.rotxoffset;
                    hand.posstart = hand.posoffset;
                }
                // BADDTOR(360) - BADDTOR3(recoilangle)
                hand.rotxend = baddtor(360.0) - recoilangle * crate::pd_spike::pdmath::M_BADTAU / 360.0;
                hand.posend.x = (xpos - hand.aimpos.x) * recoildist / 1000.0;
                hand.posend.y = 0.0;
                hand.posend.z = (posz - hand.aimpos.z) * recoildist / 1000.0;
                let mult2 = if curframe < unk24 {
                    (curframe as f32 * dtor(90.0) / unk24 as f32).sin()
                } else {
                    ((curframe - unk24) as f32 * dtor(180.0) / unk25 as f32).cos() * 0.5 + 0.5
                };
                hand.rotxoffset = pdmtx::tween_rot_axis(hand.rotxstart, hand.rotxend, mult2);
                hand.useposrot = true;
                hand.posoffset = (hand.posend - hand.posstart) * mult2 + hand.posstart;
                hand.posrotmtx = pdmtx::load_x_rotation(hand.rotxoffset);
                pdmtx::set_translation(&mut hand.posrotmtx, hand.posoffset);
            }
        }
        if curframe >= sum {
            let hand = &self.hands[h];
            if unk27 >= 0 && hand.triggerreleased && hand.triggeron {
                return true;
            } else if sum + recoverytime60 <= curframe {
                return true;
            }
        }
        false
    }

    /// `bgun_tick_inc_attacking_shoot` (`:2009`).
    fn bgun_tick_inc_attacking_shoot(&mut self, h: usize) -> bool {
        let Some(func) = self.func_of(h) else { return true };
        if self.hands[h].stateminor == HANDSTATEMINOR_ATTACK_SHOOT_0 {
            let mut ready = true;
            if self.hands[h].statecycles == 0 {
                self.hands[h].gs_barrelspeedfrac = 0.0;
                if let Some(script) = func.fire_animation {
                    self.bgun_start_animation(script, h);
                    self.hands[h].unk0cc8_01 = true;
                }
                self.hands[h].burstbullets = 0;
            }
            if !self.bgun_anim_allows_feature(h, GUNFEATURE_ATTACK) {
                ready = false;
            }
            if ready {
                self.hands[h].stateminor = HANDSTATEMINOR_ATTACK_SHOOT_1;
            }
            self.hands[h].matmot2 = self.hands[h].gs_barrelspeedfrac; // mm_reaperspeedaim
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_ATTACK_SHOOT_1 {
            let r = self.bgun_should_fire(h, &func);
            if r > 0 {
                self.bgun_fire(h, &func);
            }
            if r < 0 || r == 2 {
                self.hands[h].stateminor = HANDSTATEMINOR_ATTACK_SHOOT_2;
            }
            let hand = &mut self.hands[h];
            hand.matmot2 = hand.gs_barrelspeedfrac;
            if hand.triggeron && hand.matmot2 < 0.4 {
                hand.matmot2 = 0.4;
            }
            if hand.triggerreleased {
                hand.unk0cc8_01 = false;
            }
            return false;
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_ATTACK_SHOOT_2 {
            let mut canfireagain = if self.hands[h].stateflags & HANDSTATEFLAG_FIRED != 0 {
                let shoot = func.shoot.clone().unwrap_or_default();
                self.bgun_tick_recoil(h, &shoot)
            } else {
                true
            };
            if self.hands[h].weaponnum == WEAPON_SHOTGUN && self.hands[h].animmode == HANDANIMMODE_BUSY {
                canfireagain = false;
            }
            let hand = &mut self.hands[h];
            hand.matmot2 = hand.gs_barrelspeedfrac;
            if canfireagain && !hand.triggeron {
                hand.matmot2 = 0.0;
            }
            if hand.weaponnum == WEAPON_MAULER {
                hand.matmot1 = 0.0;
            }
            return canfireagain;
        }
        false
    }

    /// `bgun_tick_inc_attacking_throw` (`:2110`). The projectile itself is made
    /// by `hand_tick_attack` -> `bgun_create_thrown_projectile` in the world layer.
    fn bgun_tick_inc_attacking_throw(&mut self, h: usize) -> bool {
        let Some(func) = self.func_of(h) else { return true };
        if self.hands[h].stateminor == HANDSTATEMINOR_ATTACK_THROW_0 {
            if self.hands[h].statecycles == 0 {
                if func.flags & FUNCFLAG_DISCARDWEAPON != 0 {
                    // Laptop deploy / Dragon self-destruct: drop the weapon from
                    // the inventory and lower it; the throw happens when it is
                    // down (bgun_tick_inc_changegun's `throwing`).
                    let w = self.hands[h].weaponnum;
                    self.inv_remove_item_by_num(w);
                    self.ctrl.throwing = true;
                    self.bgun_switch_to_previous();
                    self.hands[h].primetimer60 = 0;
                    return true;
                }
                if let Some(script) = func.fire_animation {
                    self.bgun_start_animation(script, h);
                    self.hands[h].unk0cc8_01 = true;
                }
            }
            if func.fire_animation.is_some() {
                if self.hands[h].triggerreleased {
                    self.hands[h].unk0cc8_01 = false;
                }
                if self.bgun_anim_allows_feature(h, GUNFEATURE_ATTACK) {
                    self.hands[h].stateminor = HANDSTATEMINOR_ATTACK_THROW_1;
                    self.hands[h].unk0cc8_01 = false;
                }
            } else {
                self.hands[h].stateminor = HANDSTATEMINOR_ATTACK_THROW_1;
            }
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_ATTACK_THROW_1 {
            let hand = &mut self.hands[h];
            hand.firing = true;
            hand.attacktype = HANDATTACKTYPE_THROWPROJECTILE;
            if func.ammoindex >= 0 {
                hand.loadedammo[func.ammoindex as usize] -= 1;
            }
            hand.stateminor = HANDSTATEMINOR_ATTACK_THROW_2;
            return false;
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_ATTACK_THROW_2 {
            if self.hands[h].stateframes > func.recoverytime60 {
                return true;
            }
            if self.hands[h].weaponnum == WEAPON_REMOTEMINE
                && self.bgun_is_using_secondary_function()
                && self.hands[h].triggerreleased
                && self.hands[h].triggeron
            {
                return true;
            }
            return false;
        }
        // Only after a grenade went off in the hand: wait 4 s for the flames.
        if self.hands[h].stateminor == HANDSTATEMINOR_ATTACK_THROW_GRENADEWAIT {
            self.bgun_reset_anim(h);
            return self.hands[h].stateframes > func.activatetime60 + 240;
        }
        // Still in THROW_0 (the trigger holds the throw): cooking.
        self.hands[h].primetimer60 = self.hands[h].stateframes;
        if self.hands[h].weaponnum == WEAPON_GRENADE
            && self.hands[h].weaponfunc == FUNC_PRIMARY
            && self.hands[h].primetimer60 > func.activatetime60
        {
            let hand = &mut self.hands[h];
            hand.firing = true;
            hand.attacktype = HANDATTACKTYPE_THROWPROJECTILE;
            if func.ammoindex >= 0 {
                hand.loadedammo[func.ammoindex as usize] -= 1;
            }
            hand.stateminor = HANDSTATEMINOR_ATTACK_THROW_GRENADEWAIT;
            return false;
        }
        false
    }

    /// `bgun_tick_inc_attacking_melee` (`:2222`).
    fn bgun_tick_inc_attacking_melee(&mut self, h: usize) -> bool {
        let Some(func) = self.func_of(h) else { return true };
        if self.hands[h].weaponnum == WEAPON_REAPER {
            let lv60 = self.lv.lvupdate60freal;
            let hand = &mut self.hands[h];
            if hand.statecycles == 0 {
                hand.matmot2 = 0.1;
                hand.burstbullets = 0;
            }
            hand.firing = true;
            hand.attacktype = HANDATTACKTYPE_MELEE;
            hand.burstbullets += 1;
            if hand.triggeron {
                hand.matmot2 += 0.01 * lv60;
                if hand.matmot2 > 1.0 {
                    hand.matmot2 = 1.0;
                }
            } else {
                hand.matmot2 = 0.0;
                return true;
            }
            return false;
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_ATTACK_MELEE_0 {
            if self.hands[h].statecycles == 0 {
                self.hands[h].firing = true;
                self.hands[h].attacktype = HANDATTACKTYPE_MELEENOUNCLOAK;
                if let Some(script) = func.fire_animation {
                    self.bgun_start_animation(script, h);
                    self.hands[h].unk0cc8_01 = true;
                }
            }
            if func.fire_animation.is_some() {
                if self.hands[h].triggerreleased {
                    self.hands[h].unk0cc8_01 = false;
                }
                if self.bgun_anim_allows_feature(h, GUNFEATURE_ATTACK) {
                    self.hands[h].stateminor = HANDSTATEMINOR_ATTACK_MELEE_1;
                    self.hands[h].unk0cc8_01 = false;
                }
            } else {
                self.hands[h].stateminor = HANDSTATEMINOR_ATTACK_MELEE_1;
            }
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_ATTACK_MELEE_3 && self.bgun_anim_allows_feature(h, GUNFEATURE_ATTACKAGAIN) {
            self.hands[h].stateminor = HANDSTATEMINOR_ATTACK_MELEE_1;
            self.hands[h].unk0cc8_01 = false;
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_ATTACK_MELEE_1 {
            self.hands[h].firing = true;
            self.hands[h].attacktype = HANDATTACKTYPE_MELEE;
            if func.fire_animation.is_some() {
                if !self.bgun_anim_allows_feature(h, GUNFEATURE_ATTACKAGAIN) {
                    self.hands[h].stateminor = HANDSTATEMINOR_ATTACK_MELEE_3;
                } else {
                    self.hands[h].stateminor = HANDSTATEMINOR_ATTACK_MELEE_2;
                }
            }
            return false;
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_ATTACK_MELEE_2 {
            if !self.bgun_is_anim_busy(h) {
                return true;
            }
            return self.hands[h].stateframes > 60;
        }
        false
    }

    /// `bgun_tick_inc_attacking_special` (`:2330`).
    fn bgun_tick_inc_attacking_special(&mut self, h: usize) -> bool {
        let Some(func) = self.func_of(h) else { return true };
        let hand = &mut self.hands[h];
        if hand.stateminor == HANDSTATEMINOR_ATTACK_SPECIAL_START {
            hand.stateminor = HANDSTATEMINOR_ATTACK_SPECIAL_EXECUTE;
        }
        if hand.stateminor == HANDSTATEMINOR_ATTACK_SPECIAL_EXECUTE {
            hand.firing = true;
            hand.attacktype = func.specialfunc;
            if func.ammoindex >= 0 {
                hand.loadedammo[func.ammoindex as usize] -= 1;
            }
            hand.stateminor = HANDSTATEMINOR_ATTACK_SPECIAL_RECOVER;
            return false;
        }
        if hand.stateminor == HANDSTATEMINOR_ATTACK_SPECIAL_RECOVER {
            return hand.stateframes > func.recoverytime60;
        }
        false
    }

    /// `bgun_tick_inc_attackempty` (`:2365`): the dry-fire click.
    fn bgun_tick_inc_attackempty(&mut self, h: usize, lvupdate: i32) -> i32 {
        let weaponnum = self.hands[h].weaponnum;
        let mut playsound = false;
        let finger = matches!(
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
                | WEAPON_REAPER
                | WEAPON_TRANQUILIZER
        );
        if finger {
            // Weapons with visible finger trigger animations
            if self.hands[h].stateframes > 25 {
                self.hands[h].stateframes -= 25;
                self.hands[h].stateflags = 0;
                self.bgun_reset_anim(h);
            }
            if self.hands[h].animmode != HANDANIMMODE_BUSY {
                let mut restartedanim = false;
                if self.hands[h].stateflags & HANDSTATEFLAG_BUSY == 0 {
                    if let Some(script) = self.func_of(h).and_then(|f| f.fire_animation) {
                        self.bgun_start_animation(script, h);
                        restartedanim = true;
                    }
                }
                if !restartedanim && self.hands[h].stateframes > 25 {
                    playsound = true;
                }
            } else if self.bgun_anim_allows_feature(h, GUNFEATURE_CLICK) {
                playsound = true;
            }
        } else if self.hands[h].stateframes > 25 {
            playsound = true;
            self.hands[h].stateframes -= 25;
            self.hands[h].stateflags = 0;
            self.bgun_reset_anim(h);
        }
        self.hands[h].mode = HANDMODE_13;
        self.hands[h].count60 = 0;
        self.hands[h].count = 0;
        if playsound && self.hands[h].stateflags & HANDSTATEFLAG_BUSY == 0 {
            self.hands[h].stateflags |= HANDSTATEFLAG_BUSY;
            match weaponnum {
                WEAPON_PHOENIX | WEAPON_CALLISTO | WEAPON_FARSIGHT => {
                    // Maian weapons: a wet click, then (fall-through) the fast click.
                    self.sound(0x8080, 2.07);
                    self.sound(SFXMAP_8052_FIREEMPTY, 1.5);
                }
                WEAPON_TRANQUILIZER => self.sound(SFXMAP_8052_FIREEMPTY, 1.5),
                WEAPON_UNARMED | WEAPON_COMBATKNIFE | WEAPON_GRENADE | WEAPON_NBOMB | WEAPON_TIMEDMINE
                | WEAPON_PROXIMITYMINE | WEAPON_REMOTEMINE | WEAPON_COMBATBOOST => {}
                _ => self.sound(SFXMAP_8052_FIREEMPTY, 1.0),
            }
        }
        if !self.hands[h].triggeron {
            self.hands[h].mode = HANDMODE_NONE;
            self.hands[h].count60 = 0;
            self.hands[h].count = 0;
            if self.bgun_set_state(h, HANDSTATE_IDLE) {
                return lvupdate;
            }
            self.bgun_reset_anim(h);
        }
        0
    }

    /// `bgun_tick_inc_attack` (`:2525`).
    fn bgun_tick_inc_attack(&mut self, h: usize, lvupdate: i32) -> i32 {
        let mut finished = true;
        if let Some(func) = self.func_of(h) {
            finished = match func.kind() {
                INVENTORYFUNCTYPE_SHOOT => self.bgun_tick_inc_attacking_shoot(h),
                INVENTORYFUNCTYPE_THROW => self.bgun_tick_inc_attacking_throw(h),
                INVENTORYFUNCTYPE_MELEE => self.bgun_tick_inc_attacking_melee(h),
                INVENTORYFUNCTYPE_SPECIAL => self.bgun_tick_inc_attacking_special(h),
                _ => true,
            };
        }
        if finished {
            if self.hands[h].weaponnum == WEAPON_REAPER && self.hands[h].triggeron {
                self.hands[h].weaponfunc = FUNC_SECONDARY;
                finished = false;
            }
            if finished && self.bgun_set_state(h, HANDSTATE_IDLE) {
                return lvupdate;
            }
        }
        0
    }

    /// `bgun_is_ready_to_switch` (`:2570`).
    pub(crate) fn bgun_is_ready_to_switch(&self, h: usize) -> bool {
        let r = &self.hands[HAND_RIGHT];
        let l = &self.hands[HAND_LEFT];
        if h == HAND_RIGHT && l.inuse && l.state == HANDSTATE_AUTOSWITCH && l.stateminor == 0 {
            return false;
        }
        if self.ctrl.switchtoweaponnum >= 0 {
            return true;
        }
        if h == HAND_LEFT {
            if r.state == HANDSTATE_RELOAD || r.state == HANDSTATE_CHANGEFUNC || r.state == HANDSTATE_ATTACK {
                return false;
            }
            if l.inuse && !self.ctrl.dualwielding {
                return true;
            }
            if !l.inuse && self.ctrl.dualwielding {
                return true;
            }
        }
        false
    }

    /// `bgun_can_free_weapon` (`:2620`).
    fn bgun_can_free_weapon(&self, h: usize) -> bool {
        let hand = &self.hands[h];
        hand.state == HANDSTATE_CHANGEGUN && hand.stateminor == 2 && hand.count >= 3 && !self.ctrl.throwing
    }

    /// `bgun0f09bf44` (`:2634`): may the new gun come up?
    fn bgun_may_raise(&self, h: usize) -> bool {
        let mut result = self.bgun_is_loaded();
        if self.ctrl.switchtoweaponnum != -1 {
            result = false;
        }
        if h == HAND_LEFT && self.ctrl.dualwielding != self.hands[h].inuse {
            result = false;
        }
        if self.ctrl.gunmemnew >= 0 {
            result = false;
        }
        if self.hands[1 - h].state == HANDSTATE_RELOAD {
            result = false;
        }
        result
    }

    /// `bgun_tick_inc_changegun` (`:2662`).
    fn bgun_tick_inc_changegun(&mut self, h: usize, lvupdate: i32) -> i32 {
        let weaponnum = self.hands[h].weaponnum;
        let w = self.gset.weapon(weaponnum).cloned();
        if self.hands[h].statecycles == 0 {
            self.hands[h].pausetime60 = 0;
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_CHANGEGUN_UNEQUIP {
            let mut skipanim = false;
            if self.gset.has_flag(weaponnum, WEAPONFLAG_THROWABLE)
                && !(weaponnum == WEAPON_REMOTEMINE && h == HAND_LEFT)
                && self.bgun_get_ammo_state(FUNC_PRIMARY, h) <= GUNAMMOSTATE_DEPLETED
            {
                skipanim = true;
            }
            self.hands[h].count = 0;
            if !skipanim {
                let unequip = w.as_ref().and_then(|w| w.unequip_animation);
                if unequip.is_some()
                    && self.hands[h].inuse
                    && !(self.hands[h].ejectstate != EJECTSTATE_INACTIVE && self.hands[h].ejecttype == EJECTTYPE_GUN)
                {
                    if self.hands[h].statecycles == 0 {
                        self.bgun_start_animation(unequip.unwrap(), h);
                    } else if self.hands[h].animmode == HANDANIMMODE_IDLE {
                        self.hands[h].stateminor += 1;
                    }
                } else {
                    self.hands[h].stateflags |= HANDSTATEFLAG_00000001;
                    if self.hands[h].ejectstate == EJECTSTATE_INIT {
                        return 0;
                    }
                    self.hands[h].stateminor += 1;
                }
            } else {
                self.hands[h].stateminor += 1;
            }
            if self.hands[h].stateminor == HANDSTATEMINOR_CHANGEGUN_LOWER {
                self.hands[h].stateframes = 0;
            }
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_CHANGEGUN_LOWER {
            let mut delay = if self.mp { 12 } else { 16 };
            self.hands[h].count = 0;
            if w.as_ref().is_some_and(|w| w.unequip_animation.is_some()) && self.hands[h].stateflags & HANDSTATEFLAG_00000001 == 0 {
                delay = 1;
            }
            if !self.hands[h].inuse {
                delay = 1;
            }
            let throwing = self.ctrl.throwing
                || (self.hands[h].ejecttype == EJECTTYPE_GUN
                    && (self.hands[h].ejectstate == EJECTSTATE_INIT || self.hands[h].ejectstate == EJECTSTATE_AIRBORNE));
            if self.hands[h].stateframes >= delay {
                if !throwing {
                    self.events.push(GunEvent::FreeHeldRocket { hand: h });
                    self.hands[h].mode = HANDMODE_6;
                    self.hands[h].stateminor += 1;
                } else {
                    self.bgun_set_arm_pitch(h, max_pitch_pub());
                    // Laptop deploy etc.: throw the lowered gun as the secondary.
                    if self.ctrl.throwing && self.hands[h].inuse {
                        let hand = &mut self.hands[h];
                        hand.firing = true;
                        hand.attacktype = HANDATTACKTYPE_THROWPROJECTILE;
                        hand.weaponfunc = FUNC_SECONDARY;
                    }
                }
            } else {
                let a = self.hands[h].stateframes as f32 * max_pitch_pub() / delay as f32;
                self.bgun_set_arm_pitch(h, a);
            }
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_CHANGEGUN_LOAD {
            self.hands[h].animmode = HANDANIMMODE_IDLE;
            if self.hands[h].pausechange == 0 || self.hands[h].pausetime60 <= self.hands[h].count60 {
                if self.hands[h].mode == HANDMODE_6 {
                    if self.bgun_may_raise(h) {
                        self.hands[h].mode = HANDMODE_7;
                        if !self.hands[h].inuse && self.bgun_set_state(h, HANDSTATE_IDLE) {
                            return lvupdate;
                        }
                    }
                } else if self.bgun_is_loaded() {
                    // The new weapon's definition (the hand's gset was updated by
                    // bgun_tick_switch2 while we waited).
                    let neww = self.gset.weapon(self.hands[h].weaponnum).cloned();
                    if let Some(script) = neww.as_ref().and_then(|w| w.equip_animation) {
                        self.bgun_start_animation(script, h);
                        self.hands[h].unk0cc8_02 = true;
                    }
                    let hand = &mut self.hands[h];
                    hand.mode = HANDMODE_EQUIP;
                    hand.stateminor += 1;
                    hand.count60 = 0;
                    hand.count = 0;
                }
            }
            if self.hands[h].mode == HANDMODE_6 || self.hands[h].mode == HANDMODE_7 {
                self.bgun_set_arm_pitch(h, max_pitch_pub());
            }
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_CHANGEGUN_RAISE {
            let weaponnum = self.hands[h].weaponnum;
            let neww = self.gset.weapon(weaponnum).cloned();
            let mut delay = if self.mp { 12 } else { 23 };
            if self.gset.has_flag(weaponnum, WEAPONFLAG_00004000) {
                self.hands[h].animmode = HANDANIMMODE_IDLE;
            } else if neww.as_ref().is_some_and(|w| w.equip_animation.is_some()) {
                delay = 1;
            }
            if self.hands[h].count == 0 {
                self.bgun_load_all_clips(h);
                if self.gset.has_flag(weaponnum, WEAPONFLAG_THROWABLE)
                    && (weaponnum != WEAPON_REMOTEMINE || h != HAND_LEFT)
                    && self.bgun_get_ammo_state(FUNC_PRIMARY, h) <= GUNAMMOSTATE_DEPLETED
                    && self.bgun_set_state(h, HANDSTATE_AUTOSWITCH)
                {
                    self.hands[h].stateminor = 1;
                    return lvupdate;
                }
                self.p.doautoselect = false;
                if !self.p.isdead {
                    match weaponnum {
                        WEAPON_TRANQUILIZER => self.sound(SFXNUM_00E8_PICKUP_GUN, 1.5),
                        WEAPON_REAPER => self.sound(SFXNUM_00E8_PICKUP_GUN, 0.85),
                        WEAPON_LASER => self.sound(0x00f2, 1.0),
                        WEAPON_COMBATKNIFE => self.sound(0x00e9, 1.0),
                        WEAPON_REMOTEMINE | WEAPON_TIMEDMINE | WEAPON_PROXIMITYMINE => {
                            if h == HAND_RIGHT || weaponnum != WEAPON_REMOTEMINE {
                                self.sound(0x00eb, 1.0)
                            }
                        }
                        WEAPON_NONE | WEAPON_UNARMED | WEAPON_LAPTOPGUN | WEAPON_CROSSBOW | WEAPON_GRENADE
                        | WEAPON_NBOMB | WEAPON_COMBATBOOST => {}
                        _ => self.sound(SFXNUM_00E8_PICKUP_GUN, 1.0),
                    }
                }
            }
            if self.hands[h].count60 >= delay
                || !self.gset.has_model(weaponnum)
                || !self.gset.has_flag(weaponnum, WEAPONFLAG_00000040)
                || self.gset.has_flag(weaponnum, WEAPONFLAG_00000080)
            {
                let hand = &mut self.hands[h];
                hand.mode = HANDMODE_NONE;
                hand.stateminor += 1;
                if !self.gset.has_flag(weaponnum, WEAPONFLAG_00004000) {
                    self.hands[h].unk0cc8_02 = false;
                }
                self.hands[h].count60 = 0;
                self.hands[h].count = 0;
            } else {
                let a = (delay - self.hands[h].count60) as f32 * max_pitch_pub() / delay as f32;
                self.bgun_set_arm_pitch(h, a);
            }
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_CHANGEGUN_EQUIP {
            let weaponnum = self.hands[h].weaponnum;
            let has_equip = self.gset.weapon(weaponnum).is_some_and(|w| w.equip_animation.is_some());
            if has_equip && !self.gset.has_flag(weaponnum, WEAPONFLAG_00004000) {
                if self.hands[h].animmode == HANDANIMMODE_IDLE && self.bgun_set_state(h, HANDSTATE_IDLE) {
                    return lvupdate;
                }
            } else if self.bgun_set_state(h, HANDSTATE_IDLE) {
                return lvupdate;
            }
        }
        0
    }

    /// `bgun_tick_inc` (`:3018`).
    fn bgun_tick_inc(&mut self, h: usize, lvupdate: i32) -> i32 {
        let prevstate = self.hands[h].state;
        {
            let lv = self.lv;
            let hand = &mut self.hands[h];
            hand.firing = false;
            hand.flashon = false;
            hand.stateframes += lvupdate;
            if lv.lvupdate240 > 0 {
                hand.count60 += lv.lvupdate60;
                hand.count += 1;
            }
            hand.useposrot = false;
        }
        let result = match self.hands[h].state {
            HANDSTATE_IDLE => self.bgun_tick_inc_idle(h, lvupdate),
            HANDSTATE_RELOAD => self.bgun_tick_inc_reload(h, lvupdate),
            HANDSTATE_ATTACK => self.bgun_tick_inc_attack(h, lvupdate),
            HANDSTATE_CHANGEGUN => self.bgun_tick_inc_changegun(h, lvupdate),
            HANDSTATE_ATTACKEMPTY => self.bgun_tick_inc_attackempty(h, lvupdate),
            HANDSTATE_AUTOSWITCH => self.bgun_tick_inc_autoswitch(h, lvupdate),
            HANDSTATE_CHANGEFUNC => self.bgun_tick_inc_changefunc(h, lvupdate),
            _ => 0,
        };
        let hand = &mut self.hands[h];
        hand.statelastframe = hand.stateframes;
        if hand.state != prevstate {
            hand.statelastframe = -result;
        } else {
            hand.stateframes -= result;
            hand.statecycles += 1;
        }
        result
    }

    /// `bgun_set_state` (`:3074`).
    pub(crate) fn bgun_set_state(&mut self, h: usize, state: i32) -> bool {
        if state == HANDSTATE_CHANGEFUNC && self.func_by(h, 1 - self.hands[h].weaponfunc).is_none() {
            return false;
        }
        let hand = &mut self.hands[h];
        hand.state = state;
        hand.stateframes = 0;
        hand.stateflags = 0;
        hand.statecycles = 0;
        hand.stateminor = 0;
        hand.statelastframe = 0;
        true
    }

    /// `bgun_tick_hand` (`:3096`).
    fn bgun_tick_hand(&mut self, h: usize) {
        let mut lvupdate = self.lv.lvupdate60;
        self.hands[h].animframeinc = self.lv.lvupdate60;
        let mut i = 20;
        while i >= 0 {
            lvupdate = self.bgun_tick_inc(h, lvupdate);
            i -= 1;
            if lvupdate <= 0 {
                break;
            }
        }
    }

    // ─── gun memory (3440-4122), emulated ────────────────────────────────────

    /// `bgun_is_loaded` (`:3440`).
    pub fn bgun_is_loaded(&self) -> bool {
        self.ctrl.gunmemtype == WEAPON_NONE || (self.ctrl.gunmemnew < 0 && self.ctrl.load_steps <= 0)
    }

    /// `bgun_set_gun_mem_weapon` (`:3491`): start loading a new gun. PD loads the
    /// hand model, the gun model, its textures (3 per call) and the cartridge
    /// model across several `bgun_tick_master_load` calls — once per 8 sub-ticks
    /// (`bgun_tick_load`, `:4059`). The steps are counted from the real files so
    /// the switch latency matches.
    fn bgun_set_gun_mem_weapon(&mut self, weaponnum: i32) {
        self.ctrl.gunmemnew = weaponnum;
        let mut steps = 1; // MASTERLOADSTATE_FLUX -> HANDS
        let stem = self.gset.weapon(weaponnum).and_then(|w| w.model.clone());
        let hashands = self.gset.has_flag(weaponnum, WEAPONFLAG_HASHANDS);
        if hashands && self.ctrl.handfilenum != self.hand_model {
            steps += self.load_steps_for(&self.hand_model.clone());
        }
        steps += 1; // HANDS -> GUN
        if let Some(stem) = &stem {
            steps += self.load_steps_for(stem);
        }
        steps += 2; // CARTS: a cartridge model load + the compile step
        self.ctrl.load_steps = steps;
    }

    fn load_steps_for(&self, stem: &str) -> i32 {
        // GUNLOADSTATE_MODEL (1) + ceil(textures / 3) + GUNLOADSTATE_DLS (1)
        let ntex = self.models.get(stem).and_then(|m| m.file.as_ref()).map_or(0, |f| f.textures.len()) as i32;
        2 + (ntex + 2) / 3
    }

    /// `bgun_tick_load` (`:4059`) + `bgun_tick_master_load`, emulated.
    pub(crate) fn bgun_tick_load(&mut self) {
        if self.ctrl.gunmemnew < 0 {
            return;
        }
        let mut i = 0;
        while i < self.lv.lvupdate240 {
            self.ctrl.load_steps -= 1;
            i += 8;
        }
        if self.ctrl.load_steps <= 0 {
            let w = self.ctrl.gunmemnew;
            if self.gset.has_flag(w, WEAPONFLAG_HASHANDS) {
                self.ctrl.handfilenum = self.hand_model.clone();
            }
            self.ctrl.gunmemtype = w;
            self.ctrl.gunmemnew = -1;
            self.ctrl.load_steps = 0;
            self.instantiate_models();
        }
    }

    /// `model_init` for both hands' gun + hand models once loaded (`:3998`).
    fn instantiate_models(&mut self) {
        let w = self.ctrl.gunmemtype;
        let gundef = self.gset.weapon(w).and_then(|w| w.model.clone()).and_then(|s| self.models.get(&s).cloned());
        let handdef = if self.gset.has_flag(w, WEAPONFLAG_HASHANDS) {
            self.models.get(&self.hand_model).cloned()
        } else {
            None
        };
        for h in 0..2 {
            self.hands[h].gunmodel = gundef.clone().map(Model::new);
            self.hands[h].handmodel = handdef.clone().map(Model::new);
        }
    }

    // ─── switching (5228-5803) ───────────────────────────────────────────────

    /// `bgun_free_weapon` for the world layer (the laptop deploy).
    pub fn bgun_free_weapon_pub(&mut self, h: usize) {
        self.bgun_free_weapon(h);
    }

    /// `bgun_free_weapon` (`:5228`): put loaded rounds back into the reserve.
    fn bgun_free_weapon(&mut self, h: usize) {
        if self.hands[h].inuse {
            for i in 0..2 {
                if self.ctrl.ammotypes[i] >= 0 {
                    let spaceinclip = self.hands[h].clipsizes[i] - self.hands[h].loadedammo[i];
                    let index = match self.ctrl.weaponnum {
                        WEAPON_CROSSBOW => 0,
                        WEAPON_SHOTGUN => 1,
                        WEAPON_DY357MAGNUM => 2,
                        WEAPON_DY357LX => 3,
                        _ => -1,
                    };
                    if index != -1 {
                        self.hands[h].gunroundsspent[index as usize] = ((spaceinclip << 8) | 0xff) as u16;
                    }
                    if self.hands[h].loadedammo[i] > 0 {
                        let t = self.ctrl.ammotypes[i] as usize;
                        self.p.ammoheldarr[t] += self.hands[h].loadedammo[i];
                    }
                    self.hands[h].loadedammo[i] = 0;
                }
            }
        }
        // bondgun.c:5262. Without it the launcher's rocket, recreated by the
        // lowering frame's pose, rode along on the next gun.
        self.events.push(GunEvent::FreeHeldRocket { hand: h });
    }

    /// `bgun_tick_switch2` (`:5265`).
    fn bgun_tick_switch2(&mut self) {
        if self.ctrl.switchtoweaponnum >= 0 {
            if self.bgun_can_free_weapon(HAND_RIGHT) && self.bgun_can_free_weapon(HAND_LEFT) {
                let weaponnum = self.ctrl.weaponnum;
                let previnuse = self.hands[HAND_LEFT].inuse;
                if self.ctrl.dualwielding && !self.inv_has_double(self.ctrl.switchtoweaponnum) {
                    self.ctrl.dualwielding = false;
                }
                self.bgun_free_weapon(HAND_LEFT);
                self.bgun_free_weapon(HAND_RIGHT);
                if self.ctrl.switchtoweaponnum == WEAPON_NONE {
                    self.hands[HAND_LEFT].inuse = false;
                    self.hands[HAND_RIGHT].inuse = false;
                    self.ctrl.weaponnum = WEAPON_NONE;
                } else {
                    let s = self.ctrl.switchtoweaponnum;
                    self.bgun_set_gun_mem_weapon(s);
                    self.ctrl.weaponnum = s;
                    self.hands[HAND_LEFT].inuse = true;
                    self.hands[HAND_RIGHT].inuse = true;
                }
                if self.ctrl.weaponnum == WEAPON_REMOTEMINE {
                    self.ctrl.dualwielding = true;
                }
                if !self.ctrl.dualwielding {
                    self.hands[HAND_LEFT].inuse = false;
                }
                if (WEAPON_UNARMED..=0x2c).contains(&weaponnum) {
                    self.ctrl.prevweaponnum = weaponnum;
                }
                self.ctrl.prevwasdualwielding = previnuse;
                self.ctrl.invertgunfunc = false;
                for i in 0..2 {
                    let wn = self.ctrl.weaponnum;
                    let hand = &mut self.hands[i];
                    hand.ejectstate = EJECTSTATE_INACTIVE;
                    hand.ejecttype = EJECTTYPE_GUN;
                    hand.unk0d0f_02 = false;
                    hand.activatesecondary = false;
                    hand.matmot1 = 0.0;
                    hand.matmot2 = 0.0;
                    hand.matmot3 = 0.0;
                    hand.gunsmokepoint = 0.0;
                    hand.burstbullets = 0;
                    hand.loadslide = 0.0;
                    hand.allowshootframe = 0;
                    hand.lastshootframe60 = 0;
                    hand.weaponfunc = FUNC_PRIMARY;
                    hand.weaponnum = wn;
                    hand.gangstarot = 0.0;
                    self.bgun_init_clips(i);
                    self.hands[i].anim = super::anim::Anim::default();
                    if self.hands[i].audiohandle {
                        self.hands[i].audiohandle = false;
                        self.events.push(GunEvent::StopLoop { hand: i });
                    }
                }
                self.ctrl.switchtoweaponnum = -1;
                self.ctrl.throwing = false;
            }
        } else if ((self.hands[HAND_LEFT].inuse && !self.ctrl.dualwielding)
            || (!self.hands[HAND_LEFT].inuse && self.ctrl.dualwielding))
            && self.bgun_can_free_weapon(HAND_LEFT)
        {
            self.bgun_free_weapon(HAND_LEFT);
            self.hands[HAND_LEFT].inuse = self.ctrl.dualwielding;
        }
    }

    /// `bgun_equip_weapon` (`:5418`).
    pub fn bgun_equip_weapon(&mut self, weaponnum: i32) {
        if self.ctrl.weaponnum == weaponnum && self.ctrl.switchtoweaponnum == -1 {
            return;
        }
        self.ctrl.switchtoweaponnum = weaponnum;
        self.ctrl.wantammo = false;
    }

    fn inv_has_single(&self, weaponnum: i32) -> bool {
        weaponnum == WEAPON_UNARMED || self.p.inventory.iter().any(|(w, _)| *w == weaponnum)
    }

    fn inv_has_double(&self, weaponnum: i32) -> bool {
        self.p.inventory.iter().any(|(w, d)| *w == weaponnum && *d)
    }

    /// `bgun_get_switch_to_weapon` (`:5454`).
    fn bgun_get_switch_to_weapon(&self, h: usize) -> i32 {
        let mut w = if self.ctrl.switchtoweaponnum >= 0 { self.ctrl.switchtoweaponnum } else { self.ctrl.weaponnum };
        if !self.ctrl.dualwielding && h == HAND_LEFT {
            w = WEAPON_NONE;
        }
        w
    }

    /// `inv_choose_cycle_forward_weapon` over the spike's inventory: each weapon
    /// appears once, and a weapon held twice is offered single then dual (PD's
    /// cycle order, `inv.c`).
    fn cycle_list(&self) -> Vec<(i32, bool)> {
        let mut out = vec![(WEAPON_UNARMED, false)];
        for (w, dual) in &self.p.inventory {
            out.push((*w, false));
            if *dual {
                out.push((*w, true));
            }
        }
        out
    }

    /// `bgun_cycle_forward` / `bgun_cycle_back` (`:5494`, `:5521`).
    pub fn bgun_cycle(&mut self, forward: bool) {
        let cur = (self.bgun_get_switch_to_weapon(HAND_RIGHT), self.bgun_get_switch_to_weapon(HAND_LEFT) != WEAPON_NONE);
        let list = self.cycle_list();
        let idx = list.iter().position(|e| *e == cur).unwrap_or(0) as i32;
        let n = list.len() as i32;
        let next = list[((idx + if forward { 1 } else { -1 }).rem_euclid(n)) as usize];
        self.ctrl.dualwielding = next.1;
        self.bgun_equip_weapon(next.0);
    }

    /// Select a specific weapon (the spike's number keys), dual if held twice and
    /// requested.
    pub fn select_weapon(&mut self, weaponnum: i32, dual: bool) {
        self.ctrl.dualwielding = dual && self.inv_has_double(weaponnum);
        self.bgun_equip_weapon(weaponnum);
    }

    /// `bgun_auto_switch_weapon` (`:5669`), over the spike inventory.
    pub(crate) fn bgun_auto_switch_weapon(&mut self) {
        const PRIMARY: [i32; 35] = [
            WEAPON_RCP120, WEAPON_SUPERDRAGON, WEAPON_K7AVENGER, WEAPON_AR34, WEAPON_CALLISTO, WEAPON_LAPTOPGUN,
            WEAPON_DRAGON, WEAPON_CMP150, WEAPON_CYCLONE, WEAPON_FARSIGHT, WEAPON_SHOTGUN, WEAPON_REAPER,
            WEAPON_DY357LX, WEAPON_MAULER, WEAPON_DY357MAGNUM, WEAPON_MAGSEC4, WEAPON_PHOENIX, WEAPON_FALCON2_SCOPE,
            WEAPON_FALCON2, WEAPON_FALCON2_SILENCER, WEAPON_SNIPERRIFLE, WEAPON_CROSSBOW, WEAPON_TRANQUILIZER,
            WEAPON_LASER, WEAPON_SUPERDRAGON, WEAPON_DEVASTATOR, WEAPON_ROCKETLAUNCHER, WEAPON_SLAYER,
            WEAPON_GRENADE, WEAPON_NBOMB, WEAPON_PROXIMITYMINE, WEAPON_TIMEDMINE, WEAPON_REMOTEMINE,
            WEAPON_COMBATKNIFE, WEAPON_UNARMED,
        ];
        let cur = self.ctrl.weaponnum;
        let mut newweaponnum = -1;
        let mut firstweaponnum = -1;
        let mut foundsuperdragon = false;
        let mut foundcurrent = false;
        let mut i = 0;
        loop {
            let wn = PRIMARY[i];
            if self.inv_has_single(wn) {
                let mut usable = false;
                let f = self.gset.func(wn, FUNC_PRIMARY);
                if !self.bgun_func_unusable(f, wn) && f.is_some_and(|f| f.flags & FUNCFLAG_AUTOSWITCHUNSELECTABLE == 0) {
                    usable = true;
                }
                if wn == WEAPON_SUPERDRAGON && !foundsuperdragon {
                    foundsuperdragon = true;
                } else {
                    let f = self.gset.func(wn, FUNC_SECONDARY);
                    if !self.bgun_func_unusable(f, wn) && f.is_some_and(|f| f.flags & FUNCFLAG_AUTOSWITCHUNSELECTABLE == 0) {
                        usable = true;
                    }
                }
                if wn == cur {
                    foundcurrent = true;
                } else if usable {
                    newweaponnum = wn;
                    if firstweaponnum == -1 {
                        firstweaponnum = wn;
                    }
                }
            }
            i += 1;
            if i >= PRIMARY.len() || (newweaponnum != -1 && foundcurrent) {
                break;
            }
        }
        if !foundcurrent {
            newweaponnum = firstweaponnum;
        }
        if newweaponnum == -1 {
            newweaponnum = WEAPON_UNARMED;
        }
        if newweaponnum >= 0 && newweaponnum != cur {
            self.ctrl.dualwielding = self.inv_has_double(newweaponnum);
            self.bgun_equip_weapon(newweaponnum);
        }
    }

    /// `inv_remove_item_by_num` (`inv.c`) over the spike's inventory.
    pub fn inv_remove_item_by_num(&mut self, weaponnum: i32) {
        self.p.inventory.retain(|(w, _)| *w != weaponnum);
    }

    /// `bgun_switch_to_previous` (`:5471`, NTSC 1.0+).
    pub fn bgun_switch_to_previous(&mut self) {
        let prev = self.ctrl.prevweaponnum;
        if self.inv_has_single(prev) {
            self.ctrl.dualwielding = self.inv_has_double(prev) && self.ctrl.prevwasdualwielding;
            self.bgun_equip_weapon(prev);
        } else {
            self.bgun_auto_switch_weapon();
        }
    }

    /// `bgun_start_detonate_animation` (`:6396`): the left hand's detonator
    /// press (`var80070200`: ANIM_0434 at full speed).
    pub fn bgun_start_detonate_animation(&mut self) {
        if self.hands[HAND_LEFT].weaponnum == WEAPON_REMOTEMINE {
            if let Some(script) = self.gset.detonate_script {
                self.bgun_start_animation(script, HAND_LEFT);
            }
        }
    }

    /// `bgun_reload_if_possible` (`:5850`).
    pub fn bgun_reload_if_possible(&mut self, h: usize) {
        let w = self.bgun_get_weapon_num(h);
        let has_ammo = self.gset.weapon(w).is_some_and(|w| w.ammos[0].is_some());
        if has_ammo && self.hands[h].modenext == HANDMODE_NONE {
            self.hands[h].modenext = HANDMODE_RELOAD;
        }
    }

    /// `bgun_set_adjust_pos` (`:5860`).
    pub fn bgun_set_adjust_pos(&mut self, angle: f32) {
        let z = (1.0 - angle.cos()) * 5.0;
        self.hands[0].adjustpos.z = z;
        self.hands[1].adjustpos.z = z;
    }

    /// `bgun_start_slide` (`:5868`).
    pub(crate) fn bgun_start_slide(&mut self, h: usize) {
        self.hands[h].slideinc = true;
    }

    /// `bgun_update_slide` (`:5879`).
    pub(crate) fn bgun_update_slide(&mut self, h: usize) {
        let slidemax = self.func_of(h).and_then(|f| f.shoot).map_or(0.0, |s| s.slidemax);
        let lv = self.lv.lvupdate60freal;
        if self.hands[h].slideinc {
            let hand = &mut self.hands[h];
            if hand.slidetrans < slidemax {
                hand.slidetrans += slidemax * 0.25 * lv;
            }
            if hand.slidetrans >= slidemax {
                hand.slidetrans = slidemax;
                hand.slideinc = false;
            }
        } else if self.hands[h].loadedammo[FUNC_PRIMARY] > 0 && self.bgun_anim_allows_feature(h, GUNFEATURE_ATTACKAGAIN) {
            let hand = &mut self.hands[h];
            if hand.slidetrans > 0.0 {
                hand.slidetrans -= slidemax * 0.166_666_67 * lv;
            }
            if hand.slidetrans < 0.0 {
                hand.slidetrans = 0.0;
            }
        }
    }

    /// `bgun0f0abd30` (`:10413`): clip sizes for a newly equipped weapon.
    pub(crate) fn bgun_init_clips(&mut self, h: usize) {
        let w = self.gset.weapon(self.hands[h].weaponnum).cloned();
        for i in 0..2 {
            if h == HAND_RIGHT {
                self.ctrl.ammotypes[i] = -1;
            }
            if let Some(a) = w.as_ref().and_then(|w| w.ammos[i].as_ref()) {
                if h == HAND_RIGHT {
                    self.ctrl.ammotypes[i] = a.ammotype;
                }
                self.hands[h].clipsizes[i] = a.clipsize;
                if h == HAND_LEFT && self.hands[h].weaponnum == WEAPON_REMOTEMINE {
                    self.hands[h].clipsizes[i] = 0;
                }
                self.hands[h].loadedammo[i] = 0;
            }
        }
        self.hands[h].upgrademult = [1.0; 2];
        self.hands[h].finalmult = [1.0; 2];
    }

    // ─── functions / trigger (8937-9230) ─────────────────────────────────────

    /// `bgun_set_trigger_on` (`:8937`).
    fn bgun_set_trigger_on(&mut self, h: usize, on: bool) {
        let hand = &mut self.hands[h];
        hand.triggerprev = hand.triggeron;
        hand.triggeron = on;
        if !on {
            hand.triggerreleased = true;
        }
    }

    /// `FUNCISSEC()` (`constants.h:84`).
    pub(crate) fn funcissec(&self) -> bool {
        let w = self.ctrl.weaponnum;
        if !(WEAPON_UNARMED..=WEAPON_COMBATBOOST).contains(&w) {
            return false;
        }
        self.p.gunfuncs[((w - 1) >> 3) as usize] & (1 << ((w - 1) & 7)) != 0
    }

    fn set_func(&mut self, secondary: bool) {
        let w = self.ctrl.weaponnum;
        if !(WEAPON_UNARMED..=WEAPON_COMBATBOOST).contains(&w) {
            return;
        }
        let (i, bit) = (((w - 1) >> 3) as usize, 1u8 << ((w - 1) & 7));
        if secondary {
            self.p.gunfuncs[i] |= bit;
        } else {
            self.p.gunfuncs[i] &= !bit;
        }
    }

    /// `bgun_consider_toggle_gun_function` (`:8963`).
    pub fn bgun_consider_toggle_gun_function(&mut self, usedowntime: i32, trigpressed: bool) -> i32 {
        match self.bgun_get_weapon_num(HAND_RIGHT) {
            WEAPON_SNIPERRIFLE => {
                self.ctrl.invertgunfunc = true;
                if trigpressed {
                    return USETIMER_STOP;
                }
                if usedowntime < 50 || self.hands[HAND_RIGHT].weaponfunc != FUNC_SECONDARY {
                    return USETIMER_CONTINUE;
                }
                self.hands[HAND_RIGHT].activatesecondary = true;
                USETIMER_REPEAT
            }
            WEAPON_RCP120 | WEAPON_LAPTOPGUN | WEAPON_DRAGON | WEAPON_REMOTEMINE => {
                self.ctrl.invertgunfunc = true;
                USETIMER_STOP
            }
            WEAPON_MAULER | WEAPON_CMP150 | WEAPON_K7AVENGER | WEAPON_AR34 | WEAPON_FARSIGHT | WEAPON_TIMEDMINE => {
                if !trigpressed {
                    let sec = self.funcissec();
                    self.set_func(!sec);
                    return USETIMER_STOP;
                }
                USETIMER_CONTINUE
            }
            _ => {
                if trigpressed {
                    self.ctrl.invertgunfunc = true;
                } else {
                    let sec = self.funcissec();
                    self.set_func(!sec);
                }
                USETIMER_STOP
            }
        }
    }

    /// `bgun0f0a8c50` (`:9036`): releasing B clears a temporary invert.
    pub fn bgun_release_use(&mut self) {
        if !self.hands[HAND_RIGHT].activatesecondary {
            self.ctrl.invertgunfunc = false;
        }
    }

    /// `bgun_is_using_secondary_function` (`:9043`).
    pub fn bgun_is_using_secondary_function(&self) -> bool {
        let sec = self.funcissec();
        if sec {
            !self.ctrl.invertgunfunc
        } else {
            self.ctrl.invertgunfunc
        }
    }

    /// `bgun_tick_gameplay` (`:9073`): trigger routing (incl. dual-wield
    /// alternation) and the hand state machines.
    pub fn bgun_tick_gameplay(&mut self, triggeron: bool, lv: Lv) {
        self.lv = lv;
        let mut gunsfiring = [false, false];
        let p = &mut self.p;
        p.playertriggerprev = p.playertriggeron;
        p.playertriggeron = triggeron;
        if !triggeron && p.playertriggerprev {
            p.doautoselect = true;
        }
        if self.p.playertriggeron {
            self.p.playertrigtime240 += lv.lvupdate240;
            let cur = self.p.curguntofire;
            if self.hands[HAND_LEFT].inuse && self.hands[HAND_RIGHT].inuse && self.ctrl.weaponnum != WEAPON_REMOTEMINE {
                if self.p.playertrigtime240 > 80 {
                    gunsfiring[cur] = true;
                    if self.bgun_clip_has_ammo(1 - cur) || self.hands[1 - cur].triggeron {
                        gunsfiring[1 - cur] = true;
                    }
                } else {
                    if !self.p.playertriggerprev && (self.bgun_clip_has_ammo(1 - cur) || !self.bgun_clip_has_ammo(cur)) {
                        self.p.curguntofire = 1 - cur;
                    }
                    let cur = self.p.curguntofire;
                    gunsfiring[cur] = true;
                    gunsfiring[1 - cur] = false;
                }
            } else {
                if !self.hands[cur].inuse && self.hands[1 - cur].inuse {
                    self.p.curguntofire = 1 - cur;
                }
                if self.ctrl.weaponnum == WEAPON_REMOTEMINE {
                    self.p.curguntofire = 0;
                }
                let cur = self.p.curguntofire;
                gunsfiring[cur] = true;
                gunsfiring[1 - cur] = false;
            }
        } else {
            self.p.playertrigtime240 = 0;
        }
        self.bgun_set_trigger_on(HAND_RIGHT, gunsfiring[0]);
        self.bgun_set_trigger_on(HAND_LEFT, gunsfiring[1]);

        if lv.lvupdate240 > 0 {
            self.bgun_tick_hand(HAND_RIGHT);
            self.bgun_tick_hand(HAND_LEFT);
            self.bgun_tick_switch2();
            if self.p.unlimited_ammo {
                // CHEAT_UNLIMITEDAMMO's bgun_give_max_ammo(false).
                for t in 1..AMMO_CAPACITY.len() {
                    self.p.ammoheldarr[t] = AMMO_CAPACITY[t];
                }
            }
        }
    }

    /// `bgun_tick_mauler_charge` (`:7887`).
    pub(crate) fn bgun_tick_mauler_charge(&mut self) {
        let lv = self.lv.lvupdate60freal;
        for i in 0..2 {
            if !self.hands[i].inuse {
                continue;
            }
            let mut charging = false;
            if self.bgun_is_reloading(i) {
                self.hands[i].matmot1 = 0.0;
            } else if self.hands[i].weaponfunc == FUNC_SECONDARY {
                let hand = &mut self.hands[i];
                let oldvalue = hand.matmot1 as i32;
                if hand.loadedammo[0] >= 2 && hand.matmot1 < 5.0 {
                    charging = true;
                    hand.matmot1 += lv * 0.05;
                }
                if hand.matmot1 > 5.0 {
                    hand.matmot1 = 5.0;
                }
                let newvalue = hand.matmot1 as i32;
                if oldvalue != newvalue && hand.loadedammo[0] >= 2 {
                    hand.loadedammo[0] -= 1;
                }
            } else {
                let hand = &mut self.hands[i];
                hand.matmot1 -= lv * 0.005;
                if hand.matmot1 < 0.0 {
                    hand.matmot1 = 0.0;
                }
            }
            if !self.hands[i].audiohandle && self.hands[i].matmot1 > 0.1 && charging && self.lv.lvupdate240 != 0 {
                self.hands[i].audiohandle = true;
                self.events.push(GunEvent::Sound { id: 0x8065, speed: 0.5 });
            }
            if self.hands[i].audiohandle && (self.hands[i].matmot1 < 0.1 || !charging) {
                self.hands[i].audiohandle = false;
                self.events.push(GunEvent::StopLoop { hand: i });
            }
        }
    }

    /// `gset_get_xpos` (`gset.c:155`).
    pub(crate) fn gset_get_xpos(&self, h: usize) -> f32 {
        let w = self.gset.weapon(self.bgun_get_weapon_num(h));
        let x = w.map_or(0.0, |w| w.posx);
        if h == 0 {
            x
        } else {
            -x
        }
    }

    /// Give the player a weapon + its MP starting ammo (`inv_give_single_weapon` /
    /// `inv_give_double_weapon` + the MP setup's ammo).
    pub fn give_weapon(&mut self, weaponnum: i32, double: bool) {
        if let Some(e) = self.p.inventory.iter_mut().find(|(w, _)| *w == weaponnum) {
            e.1 |= double;
        } else {
            self.p.inventory.push((weaponnum, double));
        }
        if let Some(w) = self.gset.weapon(weaponnum) {
            for (t, q) in w.mp_ammo {
                if t > 0 && (t as usize) < self.p.ammoheldarr.len() {
                    let cap = AMMO_CAPACITY.get(t as usize).copied().unwrap_or(999);
                    self.p.ammoheldarr[t as usize] = (self.p.ammoheldarr[t as usize] + q).min(cap);
                }
            }
        }
    }
}

/// `MAX_PITCH` for the sibling module.
pub(crate) fn max_pitch_pub() -> f32 {
    baddtor(50.0)
}
