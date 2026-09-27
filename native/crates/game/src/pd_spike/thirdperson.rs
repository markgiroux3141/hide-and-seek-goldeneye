//! `player_choose_third_person_animation` (`player.c:5472`) — the single function
//! that animates a bot's body — and its table `var80070ba4` (`player.c:4155`).
//!
//! Bots never enter PD's guard `ACT_ATTACK` and never play an attack clip. Standing,
//! walking, running and shooting are all this: seven rows per wield mode — standing
//! (no turn / soft turn / hard turn), ducking and squatting (still / moving) — picked
//! from the bot's crouch position and how fast it moves relative to where it faces,
//! with the legs twisted towards the travel direction (±60°, ±30° squatting) and the
//! clip played **backwards** when the bot moves away from where it faces.
//!
//! The rows' `attackanimconfig`s are carried only for what a bot uses them for:
//! the idle clip number and the vertical aim limits (`maxup`/`maxdown` + the
//! free-arm fractions, read by `chr_calculate_aimend_vertical`).
//!
//! The crouch position is `bot_guess_crouch_pos` (`bot.c:748`): the chr's height,
//! which `chr_update_position` lowers near `GEOFLAG_AIBOTDUCK`/`AIBOTCROUCH` tiles
//! and on go-tos to `PADFLAG_AIDUCK`/`AICROUCH` pads.

use super::anims::{self, AnimId};
use super::model::Anim;
use super::pdmath::baddtor;

/// The fields of `struct attackanimconfig` a bot reads.
#[derive(Clone, Copy, Debug)]
pub struct AttackAnimConfig {
    pub animnum: AnimId,
    pub maxup: f32,
    pub maxdown: f32,
    pub maxleft: f32,
    pub maxright: f32,
    pub freearmfracup: f32,
    pub freearmfracdown: f32,
}

const fn cfg(animnum: AnimId, up: f32, down: f32, left: f32, right: f32, fu: f32, fd: f32) -> AttackAnimConfig {
    // Degrees here; converted with BADDTOR at use (`baddtor` is not const).
    AttackAnimConfig { animnum, maxup: up, maxdown: down, maxleft: left, maxright: right, freearmfracup: fu, freearmfracdown: fd }
}

/// `var80065be0[0]` (`chraction.c:980`) — pistol stand.
pub const CFG_STAND_PISTOL: AttackAnimConfig = cfg(anims::ANIM_0041, 50.0, -40.0, 40.0, -40.0, 0.0, 0.0);
/// `var800656c0[0]` (`chraction.c:912`) — heavy stand.
pub const CFG_STAND_HEAVY: AttackAnimConfig = cfg(anims::ANIM_0002, 50.0, -30.0, 60.0, -20.0, 1.6, 1.8);
/// `var800663d8[0]` (`chraction.c:1063`) — dual stand.
pub const CFG_STAND_DUAL: AttackAnimConfig = cfg(anims::ANIM_007A, 50.0, -40.0, 40.0, -40.0, 0.0, 0.0);
/// `g_WalkAttackAnims[0..6]` (`chraction.c:1299`).
pub const CFG_WALK_HEAVY: AttackAnimConfig = cfg(anims::ANIM_0030, 50.0, -30.0, 30.0, -30.0, 1.4, 1.3);
pub const CFG_RUN_HEAVY: AttackAnimConfig = cfg(anims::ANIM_0031, 50.0, -30.0, 30.0, -30.0, 1.1, 1.2);
pub const CFG_WALK_PISTOL: AttackAnimConfig = cfg(anims::ANIM_0052, 50.0, -30.0, 30.0, -30.0, 0.0, 0.0);
pub const CFG_RUN_PISTOL: AttackAnimConfig = cfg(anims::ANIM_0055, 50.0, -30.0, 30.0, -30.0, 0.0, 0.0);
pub const CFG_WALK_DUAL: AttackAnimConfig = cfg(anims::ANIM_006C, 50.0, -30.0, 30.0, -30.0, 0.0, 0.0);
pub const CFG_RUN_DUAL: AttackAnimConfig = cfg(anims::ANIM_006E, 50.0, -30.0, 30.0, -30.0, 0.0, 0.0);
/// The crouch rows' configs (`player.c:4140-4145`): `var800709f4` pistol duck,
/// `var80070a3c` pistol squat, `var80070a84` heavy duck, `var80070acc` heavy squat,
/// `var80070b14` dual duck, `var80070b5c` dual squat.
pub const CFG_DUCK_PISTOL: AttackAnimConfig = cfg(anims::ANIM_0281, 20.0, -90.0, 90.0, -90.0, 0.0, 0.0);
pub const CFG_SQUAT_PISTOL: AttackAnimConfig = cfg(anims::ANIM_0285, 20.0, -90.0, 90.0, -90.0, 0.0, 0.0);
pub const CFG_DUCK_HEAVY: AttackAnimConfig = cfg(anims::ANIM_0282, 20.0, -90.0, 90.0, -90.0, 1.6, 1.6);
pub const CFG_SQUAT_HEAVY: AttackAnimConfig = cfg(anims::ANIM_0286, 10.0, -90.0, 90.0, -90.0, 1.6, 1.6);
pub const CFG_DUCK_DUAL: AttackAnimConfig = cfg(anims::ANIM_0283, 20.0, -90.0, 90.0, -90.0, 0.0, 0.0);
pub const CFG_SQUAT_DUAL: AttackAnimConfig = cfg(anims::ANIM_0287, 10.0, -90.0, 90.0, -90.0, 0.0, 0.0);

impl AttackAnimConfig {
    pub fn maxup_rad(&self) -> f32 {
        baddtor(self.maxup)
    }
    pub fn maxdown_rad(&self) -> f32 {
        baddtor(self.maxdown)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WieldMode {
    Pistol = 0,
    Heavy = 1,
    Unarmed = 2,
    DualGuns = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnMode {
    StandNoTurn = 0,
    StandSoftTurn = 1,
    StandHardTurn = 2,
    DuckNoTurn = 3,
    DuckTurn = 4,
    SquatNoTurn = 5,
    SquatTurn = 6,
}

/// `CROUCHPOS_*` (`constants.h:752`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrouchPos {
    Squat = 0,
    Duck = 1,
    Stand = 2,
}

/// `bot_guess_crouch_pos` (`bot.c:748`).
pub fn bot_guess_crouch_pos(height: f32) -> CrouchPos {
    if height <= 90.0 {
        CrouchPos::Squat
    } else if height <= 135.0 {
        CrouchPos::Duck
    } else {
        CrouchPos::Stand
    }
}

/// One `struct var80070ba4` row.
#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub animcfg: Option<&'static AttackAnimConfig>,
    pub animnum: Option<AnimId>,
    pub speed: f32,
    pub startframe: f32,
    pub endframe: f32,
    /// `unk14` — the leg-twist limit, in degrees here (BADDTOR at use).
    pub unk14: f32,
}

const fn row(
    animcfg: Option<&'static AttackAnimConfig>,
    animnum: Option<AnimId>,
    speed: f32,
    startframe: f32,
    endframe: f32,
    unk14: f32,
) -> Row {
    Row { animcfg, animnum, speed, startframe, endframe, unk14 }
}

/// `var80070ba4[wieldmode][turnmode]` (`player.c:4155`). The still crouch rows hold
/// frame 0 (speed 0.001, loop 0..0.1); the moving ones play the walk.
pub const ROWS: [[Row; 7]; 4] = [
    [
        row(Some(&CFG_STAND_PISTOL), None, 0.1, 79.0, 87.0, 60.0),
        row(Some(&CFG_WALK_PISTOL), None, 0.5, -1.0, -1.0, 60.0),
        row(Some(&CFG_RUN_PISTOL), None, 0.5, -1.0, -1.0, 60.0),
        row(Some(&CFG_DUCK_PISTOL), None, 0.001, 0.0, 0.1, 60.0),
        row(Some(&CFG_DUCK_PISTOL), None, 0.503, -1.0, -1.0, 60.0),
        row(Some(&CFG_SQUAT_PISTOL), None, 0.001, 0.0, 0.1, 30.0),
        row(Some(&CFG_SQUAT_PISTOL), None, 0.45, -1.0, -1.0, 30.0),
    ],
    [
        row(Some(&CFG_STAND_HEAVY), None, 0.05, 35.0, 40.0, 60.0),
        row(Some(&CFG_WALK_HEAVY), None, 0.5, -1.0, -1.0, 60.0),
        row(Some(&CFG_RUN_HEAVY), None, 0.5, -1.0, -1.0, 60.0),
        row(Some(&CFG_DUCK_HEAVY), None, 0.001, 0.0, 0.1, 60.0),
        row(Some(&CFG_DUCK_HEAVY), None, 0.503, -1.0, -1.0, 60.0),
        row(Some(&CFG_SQUAT_HEAVY), None, 0.001, 0.0, 0.1, 30.0),
        row(Some(&CFG_SQUAT_HEAVY), None, 0.45, -1.0, -1.0, 30.0),
    ],
    [
        row(None, Some(anims::ANIM_006A), 0.25, 0.0, -1.0, 60.0),
        row(None, Some(anims::ANIM_006B), 0.5, -1.0, -1.0, 60.0),
        row(None, Some(anims::ANIM_RUNNING_ONEHANDGUN), 0.5, -1.0, -1.0, 60.0),
        row(None, Some(anims::ANIM_0280), 0.001, 0.0, 0.1, 60.0),
        row(None, Some(anims::ANIM_0280), 0.503, -1.0, -1.0, 60.0),
        row(None, Some(anims::ANIM_0284), 0.001, 0.0, 0.1, 30.0),
        row(None, Some(anims::ANIM_0284), 0.45, -1.0, -1.0, 30.0),
    ],
    [
        row(Some(&CFG_STAND_DUAL), None, 0.1, 32.0, 42.0, 60.0),
        row(Some(&CFG_WALK_DUAL), None, 0.5, -1.0, -1.0, 60.0),
        row(Some(&CFG_RUN_DUAL), None, 0.5, -1.0, -1.0, 60.0),
        row(Some(&CFG_DUCK_DUAL), None, 0.001, 0.0, 0.1, 60.0),
        row(Some(&CFG_DUCK_DUAL), None, 0.503, -1.0, -1.0, 60.0),
        row(Some(&CFG_SQUAT_DUAL), None, 0.001, 0.0, 0.1, 30.0),
        row(Some(&CFG_SQUAT_DUAL), None, 0.45, -1.0, -1.0, 30.0),
    ],
];

/// What the chooser decided, for the inspector.
#[derive(Clone, Copy, Debug)]
pub struct Choice {
    pub wieldmode: WieldMode,
    pub turnmode: Option<TurnMode>,
    /// The leg angle before slewing (radians).
    pub angle: f32,
    pub speed: f32,
    pub reconfigured: bool,
}

/// `player_choose_third_person_animation` for a living chr (the death branch lives
/// with `chr_die`, which only needs the random pick).
///
/// Mutates the model's animation and `angleoffset`, returns the row's
/// `attackanimconfig` (`*animcfgptr`) and a record of the decision.
#[allow(clippy::too_many_arguments)]
pub fn choose(
    model: &mut Anim,
    crouchpos: CrouchPos,
    wieldmode: WieldMode,
    speedsideways: f32,
    speedforwards: f32,
    speedtheta: f32,
    angleoffset: &mut f32,
    lvupdate60freal: f32,
) -> (Option<&'static AttackAnimConfig>, Choice) {
    let prevanimnum = model.animnum;
    let mut turnspeed = (speedsideways * speedsideways + speedforwards * speedforwards).sqrt();
    let speedtheta = speedtheta.abs();
    if turnspeed < speedtheta {
        turnspeed = speedtheta;
    }

    let (row, mut speed, angle, turnmode);
    if turnspeed < 0.05 {
        turnmode = match crouchpos {
            CrouchPos::Squat => TurnMode::SquatNoTurn,
            CrouchPos::Duck => TurnMode::DuckNoTurn,
            CrouchPos::Stand => TurnMode::StandNoTurn,
        };
        row = &ROWS[wieldmode as usize][turnmode as usize];
        speed = 1.0;
        angle = 0.0f32;
    } else {
        let mut a = speedsideways.atan2(speedforwards);
        if a >= baddtor(180.0) {
            a -= baddtor(360.0);
        }
        if crouchpos == CrouchPos::Squat {
            turnmode = TurnMode::SquatTurn;
            speed = (turnspeed * 2.857_142_925_262_5).min(1.2);
        } else if crouchpos == CrouchPos::Duck {
            turnmode = TurnMode::DuckTurn;
            speed = (turnspeed * 2.0).min(1.2);
        } else if turnspeed < 0.4 {
            turnmode = TurnMode::StandSoftTurn;
            speed = (2.0 * turnspeed).min(1.2);
        } else {
            turnmode = TurnMode::StandHardTurn;
            speed = turnspeed.min(1.2);
        }
        // Moving more than ~93.6° away from the facing: play the clip backwards
        // and mirror the leg angle into the front half.
        if a < -1.633_368_015_289_3 {
            a += baddtor(180.0);
            speed = -speed;
        } else if a > 1.633_368_015_289_3 {
            a -= baddtor(180.0);
            speed = -speed;
        }
        row = &ROWS[wieldmode as usize][turnmode as usize];
        let limit = baddtor(row.unk14);
        angle = a.clamp(-limit, limit);
    }

    let limit = lvupdate60freal * (baddtor(360.0) / 60.0);
    if angle - *angleoffset > limit {
        *angleoffset += limit;
    } else if angle - *angleoffset < -limit {
        *angleoffset -= limit;
    } else {
        *angleoffset = angle;
    }

    let animcfg = row.animcfg;
    let animnum = row.animnum.or(animcfg.map(|c| c.animnum)).expect("row has an animation");
    speed *= row.speed;
    let (startframe, endframe) = (row.startframe, row.endframe);

    let mut reconfigure = Some(animnum) != prevanimnum;
    if startframe >= 0.0 && (!model.looping || startframe != model.loopframe) {
        reconfigure = true;
    }
    if startframe < 0.0 && model.looping {
        reconfigure = true;
    }

    if reconfigure {
        // A merge already in flight blocks re-configuration (`animnum2 == 0` test):
        // the new row takes effect only once the current 16-tick merge finishes.
        if model.animnum2.is_none() {
            model.set_animation(animnum, if startframe >= 0.0 { startframe } else { 0.0 }, speed, 16.0);
            if startframe >= 0.0 {
                model.set_looping(startframe, 16.0);
            }
            if endframe >= 0.0 {
                model.set_end_frame(endframe);
            }
        }
    } else if speed != model.speed {
        model.set_speed(speed, 1.0);
    }

    (animcfg, Choice { wieldmode, turnmode: Some(turnmode), angle, speed, reconfigured: reconfigure })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bot_running_forwards_plays_the_run_row_at_half_speed() {
        let mut m = Anim::default();
        let mut off = 0.0;
        let (_, c) = choose(&mut m, CrouchPos::Stand, WieldMode::Heavy, 0.0, 1.0, 0.0, &mut off, 1.0);
        assert_eq!(c.turnmode, Some(TurnMode::StandHardTurn));
        assert_eq!(m.animnum, Some(anims::ANIM_0031));
        assert!((m.speed - 0.5).abs() < 1e-6);
    }

    #[test]
    fn backing_away_plays_the_run_backwards() {
        let mut m = Anim::default();
        let mut off = 0.0;
        let (_, c) = choose(&mut m, CrouchPos::Stand, WieldMode::Pistol, 0.0, -1.0, 0.0, &mut off, 1.0);
        assert!(c.speed < 0.0);
        assert_eq!(m.animnum, Some(anims::ANIM_0055));
        assert!(m.speed < 0.0);
    }

    #[test]
    fn strafing_twists_the_legs_at_six_degrees_a_tick_up_to_sixty() {
        let mut m = Anim::default();
        let mut off = 0.0;
        // Moving to the facing's side: atan2(1, 0) = 90°, clamped to the 60° row limit.
        for i in 1..=15 {
            choose(&mut m, CrouchPos::Stand, WieldMode::Heavy, 1.0, 0.0, 0.0, &mut off, 1.0);
            let expect = (baddtor(6.0) * i as f32).min(baddtor(60.0));
            assert!((off - expect).abs() < 1e-4, "tick {i}: {off} vs {expect}");
        }
    }

    #[test]
    fn a_squatting_bot_walks_the_squat_row_and_holds_frame_zero_when_still() {
        let mut m = Anim::default();
        let mut off = 0.0;
        let (cfg, c) = choose(&mut m, CrouchPos::Squat, WieldMode::Heavy, 0.0, 1.0, 0.0, &mut off, 1.0);
        assert_eq!(c.turnmode, Some(TurnMode::SquatTurn));
        assert_eq!(m.animnum, Some(anims::ANIM_0286));
        // turnspeed 1 x 2.857, capped at 1.2, x the row's 0.45.
        assert!((m.speed - 0.54).abs() < 1e-5, "{}", m.speed);
        assert_eq!(cfg.unwrap().maxup, 10.0);
        // Legs twist at most 30 degrees squatting.
        for _ in 0..20 {
            choose(&mut m, CrouchPos::Squat, WieldMode::Heavy, 1.0, 0.0, 0.0, &mut off, 1.0);
        }
        assert!((off - baddtor(30.0)).abs() < 1e-4, "{off}");
        let mut still = Anim::default();
        let mut off = 0.0;
        choose(&mut still, CrouchPos::Duck, WieldMode::Unarmed, 0.0, 0.0, 0.0, &mut off, 1.0);
        assert_eq!(still.animnum, Some(anims::ANIM_0280));
        assert!(still.looping && still.loopframe == 0.0);
        assert_eq!(bot_guess_crouch_pos(90.0), CrouchPos::Squat);
        assert_eq!(bot_guess_crouch_pos(135.0), CrouchPos::Duck);
        assert_eq!(bot_guess_crouch_pos(185.0), CrouchPos::Stand);
    }

    #[test]
    fn standing_still_loops_the_idle_window() {
        let mut m = Anim::default();
        let mut off = 0.0;
        choose(&mut m, CrouchPos::Stand, WieldMode::Heavy, 0.0, 0.0, 0.0, &mut off, 1.0);
        assert_eq!(m.animnum, Some(anims::ANIM_0002));
        assert!(m.looping);
        assert_eq!(m.loopframe, 35.0);
        assert_eq!(m.endframe, 40.0);
        assert!((m.speed - 0.05).abs() < 1e-6);
    }
}
