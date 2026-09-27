//! `bot.c` / `botact.c` — the simulant's per-tick brain and the body glue.
//!
//! Call graph per rendered frame, exactly PD's (`bot_tick`, `bot.c:909`):
//!
//! 1. `bot_tick_unpaused` — reloads, main loop → `MA_AIBOTATTACK`, the attack's
//!    `botcmd_tick_dist_mode`, `bot_choose_general_target`, and the per-hand
//!    trigger decision.
//! 2. Facing: turn `theta` towards the target (+ the zeroing error) or the travel
//!    heading, capped at 3.53°/tick, and derive `speedtheta`.
//! 3. `chr_calculate_aimend` (vertical aim only — a bot's horizontal aim IS its
//!    facing) and the forward/sideways speed multipliers.
//! 4. `bot_apply_movement` → `player_choose_third_person_animation`.
//! 5. `chr_tick` — action tick (go-to), animation, position, aim tween, flinch,
//!    and the shots whose triggers step 1 pulled.
//!
//! Nothing before `lvframe60 >= 145` (2.4 s into the match) — bots stand still at
//! the start of a PD match too.

use glam::Vec2;

use super::anims::DEATH_ANIMS;
use super::chr::{Act, Chr, MyAction, HAND_LEFT, HAND_RIGHT};
use super::chraction;
use super::pd_nav;
use super::pdmath::{baddtor, baddtor2, dtor, turn, wrap_pos};
use super::sim::Sim;
use super::thirdperson::{self, WieldMode};
use super::{botcmd, weapons};

/// `BOTDIFF_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Difficulty {
    Meat = 0,
    Easy = 1,
    Normal = 2,
    Hard = 3,
    Perfect = 4,
    Dark = 5,
}

impl Difficulty {
    pub const ALL: [Difficulty; 6] =
        [Difficulty::Meat, Difficulty::Easy, Difficulty::Normal, Difficulty::Hard, Difficulty::Perfect, Difficulty::Dark];

    pub fn label(self) -> &'static str {
        ["MeatSim", "EasySim", "NormalSim", "HardSim", "PerfectSim", "DarkSim"][self as usize]
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.to_ascii_lowercase();
        Self::ALL.into_iter().find(|d| d.label().to_ascii_lowercase().starts_with(&s))
    }

    pub fn tuning(self) -> &'static BotDifficulty {
        &G_BOT_DIFFICULTIES[self as usize]
    }
}

/// `struct botdifficulty` (`bot.c:45`).
#[derive(Clone, Copy, Debug)]
pub struct BotDifficulty {
    pub shootdelay60: i32,
    pub minzerospeed: f32,
    pub maxzerospeed: f32,
    pub zerotime60: f32,
    pub turnunzeromult: f32,
    pub forcezerominspeed: f32,
}

const fn bd(shootdelay60: i32, minz: f32, maxz: f32, zerotime60: f32, turnunzeromult: f32, forcez: f32) -> BotDifficulty {
    // Angles are in degrees here and converted with BADDTOR2 at use.
    BotDifficulty { shootdelay60, minzerospeed: minz, maxzerospeed: maxz, zerotime60, turnunzeromult, forcezerominspeed: forcez }
}

/// `g_BotDifficulties` (`bot.c:98`). `zerocloakspeed` and `dizzyamount` are left
/// out: there is no cloak and no tranquiliser here.
pub const G_BOT_DIFFICULTIES: [BotDifficulty; 6] = [
    bd(90, 15.0, 30.0, 600.0, 10.0, 20.0),
    bd(60, 7.0, 14.0, 360.0, 10.0, 8.0),
    bd(30, 4.0, 8.0, 180.0, 4.0, 5.0),
    bd(15, 1.5, 4.0, 90.0, 2.0, 2.0),
    bd(0, 0.0, 2.0, 45.0, 1.0, 0.0),
    bd(0, 0.0, 0.0, 0.0, 0.0, 0.0),
];

/// `chr_is_target_in_fov(chr, 45, false)` — the trigger cone, in radians either
/// side of `theta`. 45 is in 256ths of a turn (`D256TOR`), i.e. ±63.3°.
pub const TRIGGER_FOV_256: u8 = 45;
pub const TRIGGER_FOV_HALF: f32 = 45.0 * (360.0 * super::pdmath::M_BADPI / 180.0 / 256.0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hand {
    Right,
    Left,
}

/// The wield mode `player_choose_third_person_animation` derives from the hands.
pub fn wieldmode(c: &Chr) -> WieldMode {
    match (c.weapons_held[HAND_RIGHT], c.weapons_held[HAND_LEFT]) {
        (Some(_), Some(_)) => WieldMode::DualGuns,
        (None, None) => WieldMode::Unarmed,
        (Some(w), None) | (None, Some(w)) => {
            if weapons::get(w).map_or(false, |d| d.one_handed) {
                WieldMode::Pistol
            } else {
                WieldMode::Heavy
            }
        }
    }
}

// ─── bot_tick ────────────────────────────────────────────────────────────────

/// `bot_tick` (`bot.c:909`).
pub fn bot_tick(sim: &mut Sim, i: usize) {
    let updateable = sim.g.lvupdate240 > 0;
    if updateable && sim.g.lvframe60 >= 145 {
        // SPIKE: on Complex before stage 3 there is no route graph, so the brain
        // stays off and bots are scripted walkers (`Sim::walk_straight_to`).
        if sim.brains {
            bot_tick_unpaused(sim, i);
        }

        // Calculate target angle
        let about = bot_is_about_to_attack(sim, i);
        let target_pos = sim.chrs[i].target.map(|t| sim.chrs[t].prop_pos());
        let g = sim.g;
        let c = &mut sim.chrs[i];
        let oldangle = c.theta();
        let mut targetangle = if c.is_dead() {
            c.theta()
        } else if about {
            let tp = target_pos.unwrap();
            let a = chraction::chr_get_angle_to_pos(c, tp);
            oldangle + a + c.aibot.zeroangle
        } else {
            c.roty()
        };
        let t = turn();
        while targetangle >= t {
            targetangle -= t;
        }
        while targetangle < 0.0 {
            targetangle += t;
        }

        let tweenangle = g.lvupdate60freal * 0.061_590_049_415_827;
        let mut diffangle = targetangle - oldangle;
        if diffangle < dtor(-180.0) {
            diffangle += t;
        } else if diffangle >= dtor(180.0) {
            diffangle -= t;
        }
        let mut newangle = if diffangle >= 0.0 {
            if diffangle <= tweenangle {
                targetangle
            } else {
                let mut n = oldangle + tweenangle;
                if n >= t {
                    n -= t;
                }
                n
            }
        } else if diffangle >= -tweenangle {
            targetangle
        } else {
            let mut n = oldangle - tweenangle;
            if n < 0.0 {
                n += t;
            }
            n
        };

        let mut st = newangle - oldangle;
        if st < 0.0 {
            st += t;
        }
        if st >= dtor(180.0) {
            st -= t;
        }
        st /= g.lvupdate60freal;
        st *= 16.236_389_160_156;
        c.aibot.speedtheta = st;
        newangle = wrap_pos(newangle);
        c.aibot.lookangle = newangle;

        if c.target.is_some() && !c.aibot.ismeleeweapon {
            let left = c.has_weapon_in(HAND_LEFT);
            let right = c.has_weapon_in(HAND_RIGHT);
            let cfg = c.aibot.attackanimconfig;
            chraction::chr_calculate_aimend(c, target_pos.unwrap(), cfg, left, right);
        } else {
            chraction::chr_reset_aimend(c);
        }

        if c.actiontype == Act::Die || c.actiontype == Act::Dead {
            c.aibot.speedmultforwards = 0.0;
            c.aibot.speedmultsideways = 0.0;
        } else if c.actiontype == Act::GoPos {
            // (GOPOSFLAG_WAITING is lift logic; never set here.)
            c.aibot.speedmultforwards = 1.0;
            c.aibot.speedmultsideways = 0.0;
        } else {
            c.aibot.speedmultforwards = 0.0;
            c.aibot.speedmultsideways = 0.0;
            c.aibot.realignangleframe = g.lvframe60;
        }
    }

    bot_apply_movement(sim, i);
    chraction::chr_tick(sim, i);
}

/// `bot_is_about_to_attack(chr, false)` (`bot.c:831`): should the bot face its
/// target rather than where it's going? Yes if the target is in sight; from Easy
/// up, also if seen in the last 4 s or in the same room; from Normal up, also if
/// the rooms neighbour (or the target is in, or next to, the room it was last
/// polled in), or the route between them is 1–3 waypoints (1–4 from Hard).
/// Meat and Easy additionally need the target within 25° / 90° of travel.
pub fn bot_is_about_to_attack(sim: &Sim, i: usize) -> bool {
    let c = &sim.chrs[i];
    let Some(t) = c.target else { return false };
    let mut result = c.aibot.chrsinsight[t];
    let diff = c.aibot.config.difficulty;
    let rooms = chraction::chr_rooms(sim, i);
    let trooms = chraction::chr_rooms(sim, t);
    if diff >= Difficulty::Easy {
        if c.aibot.chrslastseen60[t] >= sim.g.lvframe60 - 240 || rooms.iter().any(|r| trooms.contains(r)) {
            result = true;
        }
        if diff >= Difficulty::Normal {
            let neighbours = |a: Option<u16>, b: Option<u16>| match (a, b) {
                (Some(a), Some(b)) => sim.level.rooms_are_neighbours(a, b),
                _ => false,
            };
            let (r0, t0) = (rooms.first().copied(), trooms.first().copied());
            let last = c.aibot.chrrooms[t];
            if neighbours(r0, t0) || (last.is_some() && last == t0) || neighbours(last, t0) {
                result = true;
            }
            let steps = c.aibot.numwaystepstotarget;
            let max = if diff == Difficulty::Normal { 3 } else { 4 };
            if (1..=max).contains(&steps) {
                result = true;
            }
        }
    }
    if matches!(diff, Difficulty::Meat | Difficulty::Easy) {
        let tp = sim.chrs[t].prop_pos();
        let mut angletotarget = (tp.x - c.pos.x).atan2(tp.z - c.pos.z) - c.roty();
        if angletotarget < 0.0 {
            angletotarget += turn();
        }
        if angletotarget > dtor(180.0) {
            angletotarget = turn() - angletotarget;
        }
        if diff == Difficulty::Meat && angletotarget > baddtor(25.0) {
            result = false;
        } else if diff == Difficulty::Easy && angletotarget > baddtor2(90.0) {
            result = false;
        }
    }
    result
}

/// `bot_apply_movement` (`bot.c:763`).
pub fn bot_apply_movement(sim: &mut Sim, i: usize) {
    let freal = sim.g.lvupdate60freal;
    // `random() % g_NumDeathAnimations` is only evaluated on the dead branch.
    let death_pick = if sim.chrs[i].is_dead() { sim.rng.random() } else { 0 };
    let c = &mut sim.chrs[i];
    let mut angle = c.theta() - c.roty();
    if angle < 0.0 {
        angle += turn();
    }
    let a = &c.aibot;
    let speedforwards = a.speedmultforwards * angle.cos() - angle.sin() * a.speedmultsideways;
    let speedsideways = a.speedmultforwards * angle.sin() + angle.cos() * a.speedmultsideways;

    if c.is_dead() {
        choose_death_animation(c, death_pick);
    } else {
        let wm = wieldmode(c);
        let speedtheta = c.aibot.speedtheta;
        let mut off = c.aibot.angleoffset;
        let crouchpos = thirdperson::bot_guess_crouch_pos(c.height);
        let (cfg, choice) =
            thirdperson::choose(&mut c.model, crouchpos, wm, speedsideways, speedforwards, speedtheta, &mut off, freal);
        c.aibot.angleoffset = off;
        c.aibot.attackanimconfig = cfg;
        c.last_choice = Some(choice);
    }
    // model_set_chr_rot_y(theta - angleoffset) is the render yaw; see `Chr::model_yaw`.
}

/// The `chr_is_dead` branch of `player_choose_third_person_animation`
/// (`player.c:5503`): keep a death animation that's already playing, else pick one
/// of `g_DeathAnimations` at random; speed 0.5, merge 16, not looping.
fn choose_death_animation(c: &mut Chr, r: u32) {
    let prev = c.model.animnum;
    let found = prev.map_or(false, |p| DEATH_ANIMS.contains(&p));
    let animnum = if found { prev.unwrap() } else { DEATH_ANIMS[(r % DEATH_ANIMS.len() as u32) as usize] };
    let speed = 0.5;
    let mut reconfigure = Some(animnum) != prev;
    if c.model.looping {
        reconfigure = true; // startframe < 0 && looping
    }
    if reconfigure {
        if c.model.animnum2.is_none() {
            c.model.set_animation(animnum, 0.0, speed, 16.0);
        }
    } else if speed != c.model.speed {
        c.model.set_speed(speed, 1.0);
    }
    c.aibot.attackanimconfig = None;
}

/// `bot_calculate_max_speed` (`bot.c:1096`), in PD units per 60 Hz tick.
pub fn bot_calculate_max_speed(c: &Chr) -> f32 {
    let mut speed = c.bodyheight * (1.0 / 159.0);
    speed = speed * 0.002_830_188_954_249 + 1.0;
    speed *= match c.aibot.config.difficulty {
        Difficulty::Meat => 5.0,
        Difficulty::Easy => 6.2,
        Difficulty::Normal => 7.6,
        Difficulty::Hard => 9.4,
        Difficulty::Perfect | Difficulty::Dark => 11.2,
    };
    // Crouched: 0.35x squatting, 0.5x ducking; else the last leg of a go-to (no
    // waypoints left) within 2 m: half speed.
    let crouchpos = thirdperson::bot_guess_crouch_pos(c.height);
    if crouchpos == thirdperson::CrouchPos::Squat {
        speed *= 0.35;
    } else if crouchpos == thirdperson::CrouchPos::Duck {
        speed *= 0.5;
    } else if c.actiontype == Act::GoPos
        && c.act_gopos.curindex >= c.act_gopos.waypoints.len()
        && Vec2::new(c.pos.x - c.act_gopos.endpos.x, c.pos.z - c.act_gopos.endpos.z).length() < 200.0
    {
        speed *= 0.5;
    }
    speed
}

/// `bot_update_lateral` (`bot.c:1152`): the smoothed per-frame displacement.
pub fn bot_update_lateral(c: &mut Chr, lvframe60: i32, numupdates: u32, lvupdate60freal: f32) -> Vec2 {
    if lvframe60 < 145 {
        return Vec2::ZERO;
    }
    let speed = bot_calculate_max_speed(c);
    let speedsideways = c.aibot.speedmultsideways * speed;
    let speedforwards = c.aibot.speedmultforwards * speed;
    let (sine, cosine) = c.roty().sin_cos();
    let sp30 = Vec2::new(
        speedsideways * cosine + speedforwards * sine,
        -speedsideways * sine + speedforwards * cosine,
    );
    let mut mv = Vec2::ZERO;
    let tmp = 0.055_000_007_152_557 * lvupdate60freal / numupdates as f32;
    for _ in 0..numupdates {
        c.aibot.moveratex = 0.945 * c.aibot.moveratex + sp30.x;
        c.aibot.moveratey = 0.945 * c.aibot.moveratey + sp30.y;
        mv.x += c.aibot.moveratex * tmp;
        mv.y += c.aibot.moveratey * tmp;
    }
    mv
}

// ─── bot_tick_unpaused ───────────────────────────────────────────────────────

/// `bot_tick_unpaused` (`bot.c:2445`), free-for-all general-sim path.
pub fn bot_tick_unpaused(sim: &mut Sim, i: usize) {
    if sim.chrs[i].is_dead() {
        return;
    }
    let g = sim.g;

    // Consider updating random values
    {
        sim.chrs[i].aibot.random2ttl60 -= g.lvupdate60;
        if sim.chrs[i].aibot.random2ttl60 < 0 {
            let a = sim.rng.random();
            let b = sim.rng.random();
            let f = sim.rng.randomfrac();
            let ab = &mut sim.chrs[i].aibot;
            ab.random2ttl60 = 1800 + (a % (60 * 240)) as i32;
            ab.random2 = b;
            ab.randomfrac = f;
        }
    }

    // Consider reloading
    for h in 0..2 {
        let c = &mut sim.chrs[i];
        if c.aibot.timeuntilreload60[h] > 0 {
            c.aibot.timeuntilreload60[h] -= g.lvupdate60;
            if c.aibot.timeuntilreload60[h] <= 0 {
                botact_reload(c, h);
            }
        } else {
            let loadedammo = c.aibot.loadedammo[h];
            let clipsize = c.aibot.weaponnum.and_then(weapons::get).map_or(0, |w| w.clip);
            if loadedammo <= 0 && clipsize > 0 {
                bot_schedule_reload(c, h);
            } else if loadedammo < clipsize / 2 && c.aibot.lastseenanytarget60 < g.lvframe60 - 120 {
                bot_schedule_reload(c, h);
            }
        }
    }

    // Main loop → pick an action. For a general sim in FFA with no pickups this
    // resolves to: has a target → attack it.
    {
        let c = &mut sim.chrs[i];
        let mut newaction = None;
        if c.myaction == MyAction::MainLoop || c.aibot.forcemainloop {
            c.aibot.forcemainloop = false;
            c.aibot.attackingplayernum = None;
            if c.target.is_some() {
                newaction = Some(MyAction::Attack);
                c.aibot.abortattacktimer60 = -1;
            }
        }
        if newaction == Some(MyAction::Attack) && c.myaction != MyAction::Attack {
            c.myaction = MyAction::Attack;
            c.aibot.distmode = None;
        }
    }

    // If the action is no longer valid, go back to the main loop
    if sim.chrs[i].myaction == MyAction::Attack {
        let c = &sim.chrs[i];
        let invalid = match c.aibot.attackingplayernum {
            Some(a) => sim.chrs[a].is_dead(),
            None => c.target.map_or(true, |t| sim.chrs[t].is_dead()),
        };
        if invalid {
            sim.chrs[i].myaction = MyAction::MainLoop;
        } else {
            botcmd::botcmd_tick_dist_mode(sim, i);
        }
    }

    bot_choose_general_target(sim, i);

    // Keep a route to the target, even if it won't be followed (`bot.c:3372`): one
    // bot per frame, round-robin. Only its length is used, by
    // `bot_is_about_to_attack`. (`botinv_tick`, weapon switching, has nothing to
    // switch to.)
    if (sim.frame_count % sim.chrs.len() as u64) as usize == i {
        if let Some(t) = sim.chrs[i].target {
            let (rooms, trooms) = (chraction::chr_rooms(sim, i), chraction::chr_rooms(sim, t));
            let first = sim.nav.waypoint_find_closest_to_pos(&sim.level, sim.chrs[i].prop_pos(), &rooms);
            let last = sim.nav.waypoint_find_closest_to_pos(&sim.level, sim.chrs[t].prop_pos(), &trooms);
            if let (Some(first), Some(last)) = (first, last) {
                let seed = pd_nav::chrnavseed(sim.g.lvframe60, i);
                let (_, n) = sim.nav.nav_find_route(last, first, 8, seed, &mut sim.rng);
                sim.chrs[i].aibot.numwaystepstotarget = n;
            }
        }
    }

    // Iterate both hands and handle shooting
    let mut firingright = false;
    for h in 0..2 {
        let mut firing = false;
        let (target, target_dead) = {
            let c = &sim.chrs[i];
            (c.target, c.target.map_or(true, |t| sim.chrs[t].is_dead()))
        };
        let target_pos = target.map(|t| sim.chrs[t].prop_pos());
        let mut punch = false;
        {
            let c = &mut sim.chrs[i];
            if c.aibot.nextbullettimer60[h] > 0 {
                c.aibot.nextbullettimer60[h] -= g.lvupdate60;
            }
            // Don't shoot the left hand on the same frame as the right
            if h == HAND_LEFT && firingright {
                c.aibot.nextbullettimer60[h] = 1;
            }
            let shootdelay60 = c.aibot.config.difficulty.tuning().shootdelay60;
            if c.aibot.changeguntimer60 <= 0 {
                if c.aibot.ismeleeweapon {
                    // Punching. `punchtimer60` is 0 idle, positive cooling down,
                    // negative "punch now".
                    if c.aibot.punchtimer60[h] >= 0 && c.aibot.timeuntilreload60[h] <= 0 {
                        let range = 210.0;
                        c.aibot.punchtimer60[h] -= g.lvupdate60;
                        if target.is_some() && c.aibot.targetinsight && c.aibot.shootdelaytimer60 >= shootdelay60 {
                            let tp = target_pos.unwrap();
                            if !chraction::chr_is_target_in_fov(c, tp, 40) || c.prop_pos().distance(tp) > range + 150.0 {
                                c.aibot.punchtimer60[h] = 0;
                            }
                        } else {
                            c.aibot.punchtimer60[h] = 0;
                        }
                        punch = c.aibot.punchtimer60[h] < 0;
                    }
                } else if c.weapons_held[h].is_some() && c.aibot.loadedammo[h] > 0 {
                    let w = weapons::get(c.aibot.weaponnum.unwrap()).unwrap();
                    let tps = weapon_get_num_ticks_per_shot(w);
                    let canshoot = if tps <= 0 { c.aibot.nextbullettimer60[h] <= 0 } else { true };
                    if canshoot {
                        if c.aibot.burstsdone[h] > 0 {
                            firing = true;
                        } else if let Some(tp) = target_pos {
                            if c.aibot.targetinsight
                                && c.aibot.shootdelaytimer60 >= shootdelay60
                                && chraction::chr_is_target_in_fov(c, tp, TRIGGER_FOV_256)
                                && !target_dead
                            {
                                firing = true;
                            }
                        }
                    }
                    if tps <= 0 && firing {
                        c.aibot.nextbullettimer60[h] = (w.unk24 as i32) + (w.unk25 as i32);
                        if w.funcflags & (weapons::FUNCFLAG_BURST3 | weapons::FUNCFLAG_BURST2) != 0
                            && c.aibot.loadedammo[h] >= 2
                        {
                            let burstqty = if w.funcflags & weapons::FUNCFLAG_BURST2 != 0 { 2 } else { 3 };
                            c.aibot.burstsdone[h] = (c.aibot.burstsdone[h] + 1) % burstqty;
                            if c.aibot.burstsdone[h] != 0 {
                                c.aibot.nextbullettimer60[h] = 5;
                            }
                        }
                    }
                } else {
                    c.aibot.burstsdone[h] = 0;
                }
            }
        }
        if punch {
            chraction::chr_punch_inflict_damage(sim, i, 2.0, 210.0);
            if h == HAND_RIGHT {
                let r = sim.rng.random();
                let c = &mut sim.chrs[i];
                c.aibot.punchtimer60[0] = match c.aibot.config.difficulty {
                    Difficulty::Meat => 120,
                    Difficulty::Easy => 60,
                    _ => 30,
                };
                if r % 3 == 0 {
                    c.aibot.punchtimer60[1] = c.aibot.punchtimer60[0] - 20;
                }
            }
        }
        if firing && h == HAND_RIGHT {
            firingright = true;
        }
        chraction::chr_set_hand_firing(&mut sim.chrs[i], h, firing);
    }
}

/// `weapon_get_num_ticks_per_shot` (`gset.c:563`): `3600 / maxrpm`, truncated.
pub fn weapon_get_num_ticks_per_shot(w: &weapons::WeaponDef) -> i32 {
    if w.automatic {
        (3600.0 / w.maxrpm) as i32
    } else {
        0
    }
}

/// `bot_schedule_reload` (`bot.c:1832`).
pub fn bot_schedule_reload(c: &mut Chr, hand: usize) {
    let Some(w) = c.aibot.weaponnum.and_then(weapons::get) else { return };
    c.aibot.timeuntilreload60[hand] = w.reloaddelay as i32 * 60;
}

/// `botact_reload` (`botact.c:1846`): instant top-up from the reserve.
pub fn botact_reload(c: &mut Chr, hand: usize) {
    c.aibot.timeuntilreload60[hand] = 0;
    if c.weapons_held[hand].is_none() {
        return;
    }
    let Some(w) = c.aibot.weaponnum.and_then(weapons::get) else { return };
    if w.clip > 0 {
        let tryamount = w.clip - c.aibot.loadedammo[hand];
        let actual = tryamount.min(c.aibot.ammoheld).max(0);
        c.aibot.ammoheld -= actual;
        c.aibot.loadedammo[hand] += actual;
    }
}

// ─── Targeting ───────────────────────────────────────────────────────────────

/// `bot_set_target` (`bot.c:1358`).
pub fn bot_set_target(sim: &mut Sim, i: usize, target: Option<usize>) {
    let g = sim.g;
    let c = &mut sim.chrs[i];
    if let Some(t) = target {
        c.aibot.targetinsight = c.aibot.chrsinsight[t];
        c.aibot.targetlastseen60 = c.aibot.chrslastseen60[t];
    } else {
        c.aibot.targetinsight = false;
        c.aibot.targetlastseen60 = -1;
    }
    if c.aibot.targetlastseen60 > c.aibot.lastseenanytarget60 {
        c.aibot.lastseenanytarget60 = c.aibot.targetlastseen60;
    }
    if c.target != target {
        c.target = target;
        c.aibot.shootdelaytimer60 = 0;
    } else if c.aibot.targetinsight {
        if g.lvupdate240 > 0 {
            c.aibot.shootdelaytimer60 += g.diffframe60;
        }
    } else {
        if g.lvupdate240 > 0 {
            c.aibot.shootdelaytimer60 -= g.diffframe60;
        }
        if c.aibot.shootdelaytimer60 < 0 {
            c.aibot.shootdelaytimer60 = 0;
        }
    }
}

/// `bot_update_zero_angle` (`bot.c:1461`).
pub fn bot_update_zero_angle(sim: &mut Sim, i: usize) {
    let g = sim.g;
    let c = &sim.chrs[i];
    let needs_roll = c.aibot.random3ttl60 - g.lvupdate60 <= 0;
    let (r1, r2) = if needs_roll { (sim.rng.random(), sim.rng.random()) } else { (0, 0) };
    let c = &mut sim.chrs[i];
    let d = c.aibot.config.difficulty.tuning();
    let a = &mut c.aibot;
    a.random3ttl60 -= g.lvupdate60;
    if a.random3ttl60 <= 0 {
        a.random3 = r1;
        a.random3ttl60 = 20 + (r2 % 20) as i32;
    }
    if g.lvupdate240 > 0 {
        if a.targetinsight {
            a.curzerotimer60 += g.diffframe60 as f32;
        } else {
            a.curzerotimer60 -= g.diffframe60 as f32;
        }
        let tmp = (d.turnunzeromult * (a.speedtheta * g.lvupdate60f)).abs();
        a.curzerotimer60 -= tmp;
    }
    if a.curzerotimer60 > a.shootdelaytimer60 as f32 {
        a.curzerotimer60 = a.shootdelaytimer60 as f32;
    }
    if a.curzerotimer60 < 0.0 {
        a.curzerotimer60 = 0.0;
    }
    let (minspeed, mut maxspeed);
    if a.curzerotimer60 >= d.zerotime60 {
        a.curzerotimer60 = d.zerotime60;
        minspeed = 0.0;
        maxspeed = 0.0;
    } else {
        let frac = (d.zerotime60 - a.curzerotimer60) / d.zerotime60;
        minspeed = baddtor2(d.minzerospeed) * frac;
        maxspeed = baddtor2(d.maxzerospeed) * frac;
    }
    if maxspeed < baddtor2(d.forcezerominspeed) {
        maxspeed = baddtor2(d.forcezerominspeed);
    }
    a.zeroinc = (maxspeed - minspeed) * (a.random3 % 0x10000) as f32 * (1.0 / 65535.0) + minspeed;
    if a.random3 & 0x10000 != 0 {
        a.zeroinc = -a.zeroinc;
    }
    for _ in 0..g.lvupdate240 {
        a.zerospeed = a.zerospeed * 0.975_000_023_841_86 + a.zeroinc;
    }
    a.zeroangle = a.zerospeed * 0.024_999_976_158_142;
}

/// `bot_choose_general_target` (`bot.c:1024`) — round-robin sight polling, then
/// "keep a target you can see, else the nearest you can see, else the nearest".
pub fn bot_choose_general_target(sim: &mut Sim, i: usize) {
    let n = sim.chrs.len();
    let g = sim.g;

    // Advance the internal pointer to the next chr and refresh stats about it
    let q = (sim.chrs[i].aibot.queryplayernum + 1) % n;
    sim.chrs[i].aibot.queryplayernum = q;
    if q != i {
        // (The canseecloaked roll consumes a random number in PD.)
        let _ = sim.rng.random();
        let dist = sim.chrs[i].prop_pos().distance(sim.chrs[q].prop_pos());
        let insight = chraction::chr_has_los_to_chr(sim, i, q);
        // SUBSTITUTION: `chr_has_los_to_chr` hands back the final room of its sight
        // ray (found through portals); without portals, the polled chr's own room.
        let room = chraction::chr_rooms(sim, q).first().copied();
        let a = &mut sim.chrs[i].aibot;
        a.chrdistances[q] = dist;
        a.chrsinsight[q] = insight;
        a.chrrooms[q] = room;
    }
    {
        let a = &mut sim.chrs[i].aibot;
        for k in 0..n {
            if a.chrsinsight[k] {
                a.chrslastseen60[k] = g.lvframe60;
            }
        }
        // chrnumsbydistanceasc: selection sort by distance
        let mut done = vec![false; n];
        for slot in 0..n {
            let mut closest: Option<(usize, f32)> = None;
            for j in 0..n {
                if !done[j] && closest.map_or(true, |(_, d)| a.chrdistances[j] < d) {
                    closest = Some((j, a.chrdistances[j]));
                }
            }
            if let Some((j, _)) = closest {
                a.chrnumsbydistanceasc[slot] = j as i32;
                done[j] = true;
            }
        }
    }

    bot_update_zero_angle(sim, i);

    // (MA_AIBOTATTACK with attackingplayernum is Kaze/scenario only.)

    // Invalidate a dead target
    if let Some(t) = sim.chrs[i].target {
        if sim.chrs[t].is_dead() {
            sim.chrs[i].target = None;
        }
    }

    if sim.chrs[i].target.is_none() {
        let diff = sim.chrs[i].aibot.config.difficulty;
        let order: Vec<i32> = sim.chrs[i].aibot.chrnumsbydistanceasc.clone();
        let mut closestavailable: Option<usize> = None;
        for &k in &order {
            if k < 0 {
                continue;
            }
            let k = k as usize;
            if k != i && !sim.chrs[k].is_dead() {
                if sim.chrs[i].aibot.chrsinsight[k] {
                    bot_set_target(sim, i, Some(k));
                    return;
                }
                if matches!(diff, Difficulty::Meat | Difficulty::Easy) {
                    bot_set_target(sim, i, Some(k));
                    return;
                }
                if closestavailable.is_none() {
                    closestavailable = Some(k);
                }
            }
        }
        bot_set_target(sim, i, closestavailable);
        return;
    }

    // Existing target still in sight: keep it
    let t = sim.chrs[i].target.unwrap();
    if sim.chrs[i].aibot.chrsinsight[t] {
        bot_set_target(sim, i, Some(t));
        return;
    }
    // Otherwise switch to the nearest chr in sight, if any
    let order: Vec<i32> = sim.chrs[i].aibot.chrnumsbydistanceasc.clone();
    for &k in &order {
        if k < 0 {
            continue;
        }
        let k = k as usize;
        if sim.chrs[i].aibot.chrsinsight[k] && k != i && !sim.chrs[k].is_dead() {
            bot_set_target(sim, i, Some(k));
            return;
        }
    }
    bot_set_target(sim, i, Some(t));
}

// ─── Spawning ────────────────────────────────────────────────────────────────

/// `bot_reset(chr, true)` (`bot.c:108`), the respawn half.
pub fn bot_reset(c: &mut Chr, nchrs: usize) {
    let config = c.aibot.config;
    c.fadealpha = -1.0;
    c.myaction = MyAction::MainLoop;
    c.damage = 0.0;
    c.target = None;
    c.firecount = [0; 2];
    c.unk32c_12 = 0;
    c.weapons_held = [None; 2];
    c.height = 185.0;
    c.hand_firing = [false; 2];
    c.gunfire_visible = [false; 2];
    c.gunpos_rendered = [None; 2];
    c.flinchcnt = -1;
    c.aibot = super::chr::Aibot::new(config, nchrs);
    c.aibot.fadeintimer60 = 120;
}

/// `bot_spawn(chr, true)` (`bot.c:262`) + the spike's loadout.
pub fn bot_spawn(sim: &mut Sim, i: usize) {
    let n = sim.chrs.len();
    bot_reset(&mut sim.chrs[i], n);
    let (pos, angle) = sim.choose_spawn_location(i);
    let c = &mut sim.chrs[i];
    c.pos = pos;
    c.prevpos = pos;
    // Spawn stands the chr on the pad's floor (`CHRCFLAG_FORCETOGROUND`, `chr.c:844`).
    c.ground = pos.y;
    c.sumground = pos.y * 9.999_998;
    c.fallspeed = glam::Vec3::ZERO;
    c.aibot.roty = angle;
    c.aibot.lookangle = angle;
    c.aibot.angleoffset = 0.0;
    c.aibot.speedtheta = 0.0;
    c.aibot.moveratex = 0.0;
    c.aibot.moveratey = 0.0;
    c.actiontype = Act::Stand;
    c.lastmoveok60 = sim.g.lvframe60;
    give_loadout(c);
}

/// SUBSTITUTION: PD bots spawn unarmed and run for weapon pickups
/// (`bot_find_default_pickup`). There are no pickups in the arena, so the
/// configured weapon is handed over the way `bot_tick_unpaused`'s weapon switch
/// does it (`chr_give_weapon` + `botact_reload`, `bot.c:2491`), with a deep
/// reserve so a match never runs dry.
pub fn give_loadout(c: &mut Chr) {
    let cfg = c.aibot.config;
    match cfg.weapon.and_then(weapons::get) {
        Some(w) => {
            c.aibot.weaponnum = Some(w.id);
            c.aibot.ismeleeweapon = false;
            c.aibot.ammoheld = 100_000;
            c.weapons_held[HAND_RIGHT] = Some(w.id);
            botact_reload(c, HAND_RIGHT);
            if cfg.dual && w.one_handed {
                c.weapons_held[HAND_LEFT] = Some(w.id);
                botact_reload(c, HAND_LEFT);
            }
        }
        None => {
            c.aibot.weaponnum = None;
            c.aibot.ismeleeweapon = true;
        }
    }
}
