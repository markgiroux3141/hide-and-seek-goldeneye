//! `botcmd.c` — how an attacking bot decides whether to close in, hold or back off.

use super::bot::Difficulty;
use super::chr::{Act, MyAction};
use super::chraction;
use super::sim::Sim;
use super::weapons;

/// `BOTDISTMODE_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DistMode {
    Backup = 1,
    Ok = 2,
    Advance = 3,
    Goto = 4,
}

impl DistMode {
    pub fn label(self) -> &'static str {
        match self {
            DistMode::Backup => "BACKUP",
            DistMode::Ok => "OK",
            DistMode::Advance => "ADVANCE",
            DistMode::Goto => "GOTO",
        }
    }
}

/// One `g_BotDistConfigs` row (PD units).
#[derive(Clone, Copy, Debug)]
pub struct DistConfig {
    pub min: f32,
    pub max: f32,
    pub limit3: f32,
}

/// `g_BotDistConfigs` (`botcmd.c:29`).
pub const BOT_DIST_CONFIGS: [DistConfig; 8] = [
    DistConfig { min: 0.0, max: 120.0, limit3: 10000.0 },   // BOTDISTCFG_CLOSE
    DistConfig { min: 300.0, max: 450.0, limit3: 4500.0 },  // BOTDISTCFG_PISTOL
    DistConfig { min: 300.0, max: 600.0, limit3: 4500.0 },  // BOTDISTCFG_DEFAULT
    DistConfig { min: 600.0, max: 1200.0, limit3: 4500.0 }, // BOTDISTCFG_SHOOTEXPLOSIVE
    DistConfig { min: 150.0, max: 250.0, limit3: 4500.0 },  // BOTDISTCFG_KAZE
    DistConfig { min: 1000.0, max: 2000.0, limit3: 3000.0 }, // BOTDISTCFG_FARSIGHT
    DistConfig { min: 0.0, max: 250.0, limit3: 10000.0 },   // BOTDISTCFG_FOLLOW
    DistConfig { min: 450.0, max: 700.0, limit3: 4500.0 },  // BOTDISTCFG_THROWEXPLOSIVE
];

pub const BOTDISTCFG_CLOSE: usize = 0;

/// `botinv_get_dist_config` for the primary function (unarmed is CLOSE).
pub fn dist_config_index(weapon: Option<weapons::WeaponId>) -> usize {
    weapon.and_then(weapons::get).map_or(BOTDISTCFG_CLOSE, |w| w.pridistconfig as usize)
}

/// `botcmd_tick_dist_mode` (`botcmd.c:61`), free-for-all branch (no follow, no Kaze).
pub fn botcmd_tick_dist_mode(sim: &mut Sim, i: usize) {
    let (confignum, prevmode, targetprop, insight) = {
        let c = &sim.chrs[i];
        let confignum = dist_config_index(c.aibot.weaponnum);
        let (t, insight) = if c.myaction == MyAction::Attack && c.aibot.attackingplayernum.is_some() {
            let a = c.aibot.attackingplayernum.unwrap();
            (Some(a), c.aibot.chrsinsight[a])
        } else {
            (c.target, c.aibot.targetinsight)
        };
        (confignum, c.aibot.distmode, t, insight)
    };
    let Some(targetprop) = targetprop else { return };
    let limits = BOT_DIST_CONFIGS[confignum];
    let target_pos = sim.chrs[targetprop].prop_pos();

    let c = &sim.chrs[i];
    // `chr_get_distance_to_coord` — full 3D, from this chr's prop position.
    let distance = c.prop_pos().distance(target_pos);
    let mut minattackdistance = limits.min;
    let mut maxattackdistance = limits.max;
    let limit3 = limits.limit3;
    match c.aibot.config.difficulty {
        Difficulty::Meat => minattackdistance *= 0.35,
        Difficulty::Easy => minattackdistance *= 0.5,
        _ => {}
    }
    match c.aibot.distmode {
        Some(DistMode::Backup) => minattackdistance += 25.0,
        Some(DistMode::Advance) | Some(DistMode::Goto) => maxattackdistance -= 25.0,
        _ => {}
    }

    let mut newmode = if distance < minattackdistance {
        DistMode::Backup
    } else if distance < maxattackdistance {
        DistMode::Ok
    } else if distance < limit3 {
        DistMode::Advance
    } else {
        DistMode::Goto
    };

    let lvupdate60 = sim.g.lvupdate60;
    let r = if newmode == DistMode::Backup && !insight { Some(sim.rng.random()) } else { None };
    let c = &mut sim.chrs[i];
    c.last_dist = distance;
    if newmode == DistMode::Backup && insight && c.aibot.distoverrideprop == Some(targetprop) {
        // don't unset
    } else {
        c.aibot.distoverrideprop = None;
        c.aibot.distoverridetimer60 = 0;
    }
    if newmode == DistMode::Ok {
        if !insight {
            newmode = DistMode::Advance;
        }
    } else if newmode == DistMode::Backup {
        // Backing up with the target out of sight turns into advancing; once it is
        // back in sight, hold OK for a random while before backing up again (stops a
        // backup/advance loop round a corner).
        if !insight {
            newmode = DistMode::Advance;
            c.aibot.distoverrideprop = Some(targetprop);
            c.aibot.distoverridetimer60 = 20 + (r.unwrap() % 120) as i32;
        } else if c.aibot.distoverrideprop.is_some() {
            if lvupdate60 < c.aibot.distoverridetimer60 {
                c.aibot.distoverridetimer60 -= lvupdate60;
                newmode = DistMode::Ok;
            } else {
                c.aibot.distoverrideprop = None;
                c.aibot.distoverridetimer60 = 0;
            }
        }
    }

    c.aibot.distmode = Some(newmode);
    if c.aibot.distmodettl60 >= 0 {
        c.aibot.distmodettl60 -= lvupdate60;
    }
    let reissue = Some(newmode) != prevmode
        || (newmode != DistMode::Ok && (c.actiontype == Act::Stand || c.aibot.distmodettl60 <= 0));
    if reissue {
        match newmode {
            DistMode::Backup => {
                chraction::chr_run_from_pos(sim, i, 10000.0, target_pos);
            }
            DistMode::Ok => {
                chraction::chr_try_stop(sim, i);
            }
            DistMode::Advance | DistMode::Goto => {
                chraction::chr_go_to_room_pos(sim, i, target_pos);
            }
        }
        sim.chrs[i].aibot.distmodettl60 = 60;
    }
}
