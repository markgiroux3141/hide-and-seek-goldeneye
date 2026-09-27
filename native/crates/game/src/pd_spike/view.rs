//! Read-only views of a [`Chr`] for the renderer and the inspector. Nothing here
//! feeds back into the simulation.

use glam::Vec3;

use super::bot::{self, Difficulty, Hand};
use super::botcmd::{self, DistConfig, DistMode};
use super::chr::{Act, Chr, MyAction, HAND_LEFT, HAND_RIGHT};
use super::chraction::{self, chr_get_flinch_amount};
use super::model::{JointFx, JointRot};
use super::pdmath::{baddtor, wrap_pos};
use super::sim::Sim;
use super::weapons::{self, WeaponId};

fn deg(r: f32) -> f32 {
    r.to_degrees()
}

impl Chr {
    /// The angle the model is drawn at: `theta - angleoffset` (`bot.c:789`).
    pub fn model_yaw(&self) -> f32 {
        wrap_pos(self.theta() - self.aibot.angleoffset)
    }

    /// 0..1 draw opacity: the death fade and the spawn fade-in (`chr.c:3393`).
    pub fn render_alpha(&self) -> f32 {
        let mut a = if self.fadealpha >= 0.0 { self.fadealpha / 255.0 } else { 1.0 };
        if self.aibot.fadeintimer60 > 0 {
            a *= (120 - self.aibot.fadeintimer60) as f32 / 120.0;
        }
        a
    }

    /// The four joint twists `chr_handle_joint_positioned` applies this frame:
    /// aim pitch at shoulders and waist, the leg-twist counter-rotation
    /// (`aibot->angleoffset`) at the waist, and the body flinch.
    pub fn joint_fx(&self) -> JointFx {
        let mut waist = JointRot { x: self.aimupback, y: self.aimsideback + self.aibot.angleoffset, z: 0.0 };
        let mut lsh = JointRot { x: self.aimuplshoulder, ..Default::default() };
        let mut rsh = JointRot { x: self.aimuprshoulder, ..Default::default() };
        let neck = JointRot { x: self.aimuprshoulder, ..Default::default() };
        if self.flinchcnt >= 0 {
            let t = self.flinchtype;
            let amount = chr_get_flinch_amount(self);
            let sh = amount * baddtor(15.0);
            for s in [&mut lsh, &mut rsh] {
                s.x -= sh;
                if t < 3 {
                    s.y -= sh;
                } else if t < 6 {
                    s.y += sh;
                }
            }
            waist.x += amount * baddtor(15.0);
            if t < 3 {
                waist.y += amount * baddtor(15.0);
            } else if t < 6 {
                waist.y -= amount * baddtor(15.0);
            }
            if matches!(t, 2 | 5 | 7) {
                waist.z += amount * baddtor(10.0);
            } else if matches!(t, 1 | 4 | 6) {
                waist.z -= amount * baddtor(10.0);
            }
        }
        JointFx { neck, waist, lshoulder: lsh, rshoulder: rsh, aimangle: chraction::chr_get_aimx_angle(self) }
    }

    pub fn held_weapons(&self) -> impl Iterator<Item = (Hand, WeaponId)> + '_ {
        [(Hand::Right, HAND_RIGHT), (Hand::Left, HAND_LEFT)]
            .into_iter()
            .filter_map(|(h, k)| self.weapons_held[k].map(|w| (h, w)))
    }

    pub fn is_moving(&self) -> bool {
        self.actiontype == Act::GoPos
    }

    pub fn target(&self) -> Option<usize> {
        self.target
    }

    /// Where a shot from the gun hand would go before spread (PD units).
    pub fn aim_ray(&self) -> (Vec3, Vec3) {
        let hand = if self.weapons_held[HAND_RIGHT].is_some() { HAND_RIGHT } else { HAND_LEFT };
        (chraction::chr_gun_pos(self, hand), chraction::chr_shot_dir(self))
    }

    pub fn is_firing(&self) -> bool {
        self.gunfire_visible.iter().any(|&f| f)
    }

    pub fn target_in_sight(&self) -> bool {
        self.aibot.targetinsight
    }

    /// The engagement band this bot measures against, difficulty-scaled.
    pub fn dist_band(&self) -> Option<DistConfig> {
        let mut c = botcmd::BOT_DIST_CONFIGS[botcmd::dist_config_index(self.aibot.weaponnum)];
        match self.aibot.config.difficulty {
            Difficulty::Meat => c.min *= 0.35,
            Difficulty::Easy => c.min *= 0.5,
            _ => {}
        }
        Some(c)
    }

    pub fn dist_mode(&self) -> Option<DistMode> {
        (self.myaction == MyAction::Attack).then_some(self.aibot.distmode).flatten()
    }

    /// The point the go-to is currently steering at.
    pub fn gopos_target_in(&self, sim: &Sim) -> Option<Vec3> {
        if self.actiontype != Act::GoPos {
            return None;
        }
        let gp = &self.act_gopos;
        Some(gp.waypoints.get(gp.curindex).map_or(gp.endpos, |&w| sim.nav.waypoint_pos(w)))
    }

    pub fn health(&self) -> f32 {
        (self.maxdamage - self.damage).max(0.0)
    }

    pub fn difficulty(&self) -> Difficulty {
        self.aibot.config.difficulty
    }

    pub fn loadout(&self) -> Option<WeaponId> {
        self.aibot.config.weapon
    }

    pub fn label_line(&self) -> String {
        let state = match self.actiontype {
            Act::Die => "DYING".to_string(),
            Act::Dead => "DEAD".to_string(),
            _ => self.dist_mode().map_or("MAINLOOP".to_string(), |m| m.label().to_string()),
        };
        let anim = self.model.animnum.map_or("-".to_string(), |a| super::anims::info(a).name.trim_start_matches("ANIM_").to_string());
        format!("{state} {anim} x{:.2}", self.model.speed)
    }

    pub fn debug_rows(&self, sim: &Sim) -> Vec<(String, String)> {
        let a = &self.aibot;
        let d = a.config.difficulty.tuning();
        let r = |k: &str, v: String| (k.to_string(), v);
        let tname = |t: Option<usize>| t.map_or("-".to_string(), |t| sim.chrs[t].name.clone());
        let mut rows = vec![
            r("actiontype", format!("{:?}", self.actiontype)),
            r("myaction", format!("{:?}", self.myaction)),
            r("distmode", self.dist_mode().map_or("-".into(), |m| m.label().into())),
            r("dist to target", format!("{:.0} cm", self.last_dist)),
            r("target", tname(self.target)),
            r("targetinsight", a.targetinsight.to_string()),
            r("shootdelay60", format!("{} / {}", a.shootdelaytimer60, d.shootdelay60)),
            r("curzerotimer60", format!("{:.0} / {:.0}", a.curzerotimer60, d.zerotime60)),
            r("zeroangle", format!("{:+.2}°", deg(a.zeroangle))),
            r("theta (facing)", format!("{:.1}°", deg(self.theta()))),
            r("roty (travel)", format!("{:.1}°", deg(self.roty()))),
            r("speedtheta", format!("{:+.2}", a.speedtheta)),
            r("angleoffset (legs)", format!("{:+.1}°", deg(a.angleoffset))),
            r("speed fwd/side", format!("{:.0} / {:.0}", a.speedmultforwards, a.speedmultsideways)),
            r("max speed", format!("{:.2} cm/tick", bot::bot_calculate_max_speed(self))),
            r("aim up (r sh / back)", format!("{:+.1}° / {:+.1}°", deg(self.aimuprshoulder), deg(self.aimupback))),
        ];
        if let Some(c) = self.last_choice {
            rows.push(r("wield / turn", format!("{:?} / {:?}", c.wieldmode, c.turnmode.unwrap())));
            rows.push(r("leg angle target", format!("{:+.1}°", deg(c.angle))));
        }
        let m = &self.model;
        rows.push(r(
            "anim",
            m.animnum.map_or("-".into(), |x| format!("{} f{:.1}/{:.0} x{:.2}", super::anims::info(x).name, m.frame, m.end_frame(), m.speed)),
        ));
        if let Some(a2) = m.animnum2 {
            rows.push(r("merging from", format!("{} ({:.0}%)", super::anims::info(a2).name, m.fracmerge * 100.0)));
        }
        rows.push(r("loop", if m.looping { format!("from f{:.0}", m.loopframe) } else { "no".into() }));
        rows.push(r(
            "weapon",
            a.weaponnum.and_then(weapons::get).map_or("Unarmed (fists)".into(), |w| w.name.to_string()),
        ));
        rows.push(r("ammo R / L", format!("{} / {}", a.loadedammo[0], a.loadedammo[1])));
        rows.push(r("reload in", format!("{} / {}", a.timeuntilreload60[0], a.timeuntilreload60[1])));
        rows.push(r("health", format!("{:.2} / {:.0}", self.health(), self.maxdamage)));
        rows.push(r("flinchcnt", format!("{} (type {})", self.flinchcnt, self.flinchtype)));
        rows.push(r("shove", format!("{:.2}", a.shotspeed.length())));
        rows.push(r("root height", format!("{:.0} cm", self.root_height())));
        if self.actiontype == Act::GoPos {
            let gp = &self.act_gopos;
            rows.push(r("route", format!("wp {}/{}", gp.curindex, gp.waypoints.len())));
        }
        rows.push(r("sight table", {
            let mut s = String::new();
            for (k, &v) in a.chrsinsight.iter().enumerate() {
                if k != sim.chrs.iter().position(|c| std::ptr::eq(c, self)).unwrap_or(usize::MAX) {
                    s.push(if v { '●' } else { '○' });
                } else {
                    s.push('·');
                }
            }
            s
        }));
        rows.push(r("K / D", format!("{} / {}", self.kills, self.deaths)));
        rows
    }
}
