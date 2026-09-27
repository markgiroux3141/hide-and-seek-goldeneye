//! `struct chrdata` + `struct aibot`, trimmed to the fields a free-for-all bot uses.
//! Field names are PD's so the port reads against the decomp line by line.

use glam::{Vec2, Vec3};

use super::anims;
use super::bot::Difficulty;
use super::botcmd::DistMode;
use super::debug_draw::Rgb;
use super::model::Anim;
use super::root_y::ROOT_Y;
use super::thirdperson::{AttackAnimConfig, Choice};
use super::weapons::WeaponId;

pub const HAND_RIGHT: usize = 0;
pub const HAND_LEFT: usize = 1;

/// `chr->actiontype` values a bot passes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Stand,
    GoPos,
    Die,
    Dead,
}

/// `chr->myaction` values a free-for-all general sim uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MyAction {
    MainLoop,
    Attack,
}

/// `chr->act_gopos`.
#[derive(Clone, Debug, Default)]
pub struct GoPos {
    pub endpos: Vec3,
    /// `act_gopos.waypoints[MAX_CHRWAYPOINTS]` up to its NULL: the loaded part of
    /// the route (at most 5). `chr_gopos_advance_waypoint` reloads it from the
    /// current waypoint once `curindex` passes 3.
    pub waypoints: Vec<usize>,
    pub curindex: usize,
    /// `act_gopos.target`: the route's last waypoint.
    pub target: Option<usize>,
    /// `GOPOSFLAG_INIT`.
    pub init: bool,
    /// `GOPOSFLAG_CROUCH` / `GOPOSFLAG_DUCK`, set on the way to a pad flagged
    /// `PADFLAG_AICROUCH` / `AIDUCK` and kept for the rest of the go-to.
    pub crouch: bool,
    pub duck: bool,
    /// `act_gopos.waydata.age`.
    pub age: i32,
}

/// The spike's per-bot setup — PD's `mpbotconfig` plus the loadout that stands in
/// for weapon pickups.
#[derive(Clone, Copy, Debug)]
pub struct BotConfig {
    pub difficulty: Difficulty,
    /// Given on every spawn (PD bots spawn unarmed and run for pickups; there are
    /// no pickups here). `None` = unarmed, which fights with fists.
    pub weapon: Option<WeaponId>,
    /// Dual-wield (only for `WEAPONFLAG_DUALWIELD` weapons).
    pub dual: bool,
}

/// `struct aibot`.
#[derive(Clone, Debug)]
pub struct Aibot {
    pub config: BotConfig,
    pub weaponnum: Option<WeaponId>,
    pub ismeleeweapon: bool,
    pub loadedammo: [i32; 2],
    /// `ammoheld[]` for the current weapon (reserve).
    pub ammoheld: i32,
    pub timeuntilreload60: [i32; 2],
    pub nextbullettimer60: [i32; 2],
    pub burstsdone: [u8; 2],
    pub punchtimer60: [i32; 2],
    pub changeguntimer60: i32,
    pub attackanimconfig: Option<&'static AttackAnimConfig>,
    pub speedmultforwards: f32,
    pub speedmultsideways: f32,
    pub speedtheta: f32,
    pub angleoffset: f32,
    pub lookangle: f32,
    pub roty: f32,
    pub moveratex: f32,
    pub moveratey: f32,
    pub distmode: Option<DistMode>,
    pub distmodettl60: i32,
    pub distoverrideprop: Option<usize>,
    pub distoverridetimer60: i32,
    pub attackingplayernum: Option<usize>,
    pub abortattacktimer60: i32,
    pub forcemainloop: bool,
    pub shotspeed: Vec3,
    pub shootdelaytimer60: i32,
    pub targetlastseen60: i32,
    pub lastseenanytarget60: i32,
    pub targetinsight: bool,
    pub queryplayernum: usize,
    pub chrnumsbydistanceasc: Vec<i32>,
    pub chrdistances: Vec<f32>,
    pub chrsinsight: Vec<bool>,
    pub chrslastseen60: Vec<i32>,
    pub zeroangle: f32,
    pub zerospeed: f32,
    pub zeroinc: f32,
    pub random3ttl60: i32,
    pub random3: u32,
    pub curzerotimer60: f32,
    pub random2ttl60: i32,
    pub random2: u32,
    pub randomfrac: f32,
    pub realignangleframe: i32,
    /// `aibot->chrrooms[]`: the room each chr was in when last polled for sight.
    pub chrrooms: Vec<Option<u16>>,
    /// `aibot->numwaystepstotarget`: PD's count (terminator included) of the route
    /// from the target back to this bot, refreshed round-robin.
    pub numwaystepstotarget: i32,
    /// Spawn fade-in: render alpha × `(120 - fadeintimer60) / 120` (`chr.c:3393`).
    pub fadeintimer60: i32,
}

impl Aibot {
    pub fn new(config: BotConfig, nchrs: usize) -> Self {
        Aibot {
            config,
            weaponnum: None,
            ismeleeweapon: true,
            loadedammo: [0; 2],
            ammoheld: 0,
            timeuntilreload60: [0; 2],
            nextbullettimer60: [0; 2],
            burstsdone: [0; 2],
            punchtimer60: [0, -1],
            changeguntimer60: 0,
            attackanimconfig: None,
            speedmultforwards: 0.0,
            speedmultsideways: 0.0,
            speedtheta: 0.0,
            angleoffset: 0.0,
            lookangle: 0.0,
            roty: 0.0,
            moveratex: 0.0,
            moveratey: 0.0,
            distmode: None,
            distmodettl60: 0,
            distoverrideprop: None,
            distoverridetimer60: 0,
            attackingplayernum: None,
            abortattacktimer60: -1,
            forcemainloop: false,
            shotspeed: Vec3::ZERO,
            shootdelaytimer60: 0,
            targetlastseen60: -1,
            lastseenanytarget60: -1,
            targetinsight: false,
            queryplayernum: 0,
            chrnumsbydistanceasc: vec![-1; nchrs],
            chrdistances: vec![u32::MAX as f32; nchrs],
            chrsinsight: vec![false; nchrs],
            chrslastseen60: vec![-1; nchrs],
            zeroangle: 0.0,
            zerospeed: 0.0,
            zeroinc: 0.0,
            random3ttl60: 0,
            random3: 0,
            curzerotimer60: 0.0,
            random2ttl60: 0,
            random2: 0,
            randomfrac: 0.0,
            realignangleframe: 0,
            chrrooms: vec![None; nchrs],
            numwaystepstotarget: 0,
            fadeintimer60: 0,
        }
    }
}

/// `struct chrdata` (the bot-relevant subset).
#[derive(Clone, Debug)]
pub struct Chr {
    pub name: String,
    pub body: usize,
    pub color: Rgb,
    /// `g_HeadsAndBodies[].height` (drives `bot_calculate_max_speed`).
    pub bodyheight: f32,
    /// `g_HeadsAndBodies[].animscale` (scales the clip's root translation).
    pub animscale: f32,

    /// Ground position (PD units). PD's `prop->pos` is this plus the animated
    /// root height — see [`Chr::prop_pos`].
    pub pos: Vec3,
    pub prevpos: Vec3,
    pub radius: f32,
    pub height: f32,

    pub actiontype: Act,
    pub act_gopos: GoPos,
    pub myaction: MyAction,
    /// `act_dead.fadetimer60`.
    pub fadetimer60: i32,
    pub fadealpha: f32,
    pub lastmoveok60: i32,
    /// `chr->invalidmove`: 0 the last move was clear, 2 it slid along an obstacle,
    /// 1 it was refused (`chr_calculate_push_pos`).
    pub invalidmove: u8,
    // `pos.y` is `chr->manground`: the smoothed height the chr stands at.
    /// `chr->ground`: the floor found under the cylinder this frame.
    pub ground: f32,
    /// `chr->sumground`: the low-pass accumulator behind `manground` (×10).
    pub sumground: f32,
    /// `chr->fallspeed`.
    pub fallspeed: Vec3,
    /// `chr->floorroom`.
    pub floorroom: Option<u16>,
    /// `chr->onladder`.
    pub onladder: bool,

    pub target: Option<usize>,
    pub damage: f32,
    pub maxdamage: f32,
    pub flinchcnt: i32,
    /// `(hidden2 >> 13) & 7`.
    pub flinchtype: u8,

    pub aimendlshoulder: f32,
    pub aimendrshoulder: f32,
    pub aimendback: f32,
    pub aimendsideback: f32,
    pub aimuplshoulder: f32,
    pub aimuprshoulder: f32,
    pub aimupback: f32,
    pub aimsideback: f32,
    pub aimendcount: i32,

    pub model: Anim,
    pub weapons_held: [Option<WeaponId>; 2],
    /// `CHRHFLAG_FIRINGRIGHT` / `LEFT` — the trigger, consumed by `chr_tick_shots`.
    pub hand_firing: [bool; 2],
    /// `chr_set_firing` — the muzzle flash is showing this tick.
    pub gunfire_visible: [bool; 2],
    pub firecount: [i32; 2],
    pub unk32c_12: u8,
    /// Muzzle positions from the last rendered frame (`chr_get_gun_pos` reads the
    /// gun model's `CHRGUNFIRE` node, which only exists once it has been drawn).
    pub gunpos_rendered: [Option<Vec3>; 2],

    pub aibot: Aibot,

    pub kills: u32,
    pub deaths: u32,
    pub suicides: u32,
    /// Last `player_choose_third_person_animation` decision (inspector only).
    pub last_choice: Option<Choice>,
    /// Last `botcmd_tick_dist_mode` distance (inspector only).
    pub last_dist: f32,
}

impl Chr {
    /// `chr_is_dead`.
    pub fn is_dead(&self) -> bool {
        matches!(self.actiontype, Act::Die | Act::Dead)
    }

    /// `prop->pos`: the root joint's position — ground plus the current clip's root
    /// height, scaled by the body's `animscale` (`chr.c:576`).
    pub fn prop_pos(&self) -> Vec3 {
        self.pos + Vec3::Y * self.root_height()
    }

    pub fn root_height(&self) -> f32 {
        let Some(a) = self.model.animnum else { return 100.0 };
        let track = ROOT_Y[anims::index_of(a)];
        let n = track.len() as i32;
        let y = |f: i32| track[f.rem_euclid(n) as usize] as f32;
        let raw = y(self.model.framea) + (y(self.model.frameb) - y(self.model.framea)) * self.model.frac;
        raw * 0.1 * self.animscale
    }

    pub fn pos2(&self) -> Vec2 {
        Vec2::new(self.pos.x, self.pos.z)
    }

    /// `chr_get_theta` for a bot: `aibot->lookangle`.
    pub fn theta(&self) -> f32 {
        self.aibot.lookangle
    }

    /// `chr_get_rot_y` for a bot: `aibot->roty`, the travel heading.
    pub fn roty(&self) -> f32 {
        self.aibot.roty
    }

    pub fn has_weapon_in(&self, hand: usize) -> bool {
        self.weapons_held[hand].is_some()
    }
}
