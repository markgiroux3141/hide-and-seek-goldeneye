//! The headless match: arena + chrs + PD's frame timing. The viewer calls
//! [`Sim::frame`] once per simulated N64 frame; tests call it in a loop.

use glam::{Vec2, Vec3};

use super::arena::Arena;
use super::pd_tiles::PdStage;
use super::tile_level::TileLevel;
use super::bot::{self, Difficulty};
use super::chr::{Act, Aibot, BotConfig, Chr, GoPos, MyAction};
use super::debug_draw::Rgb;
use super::model::Anim;
use super::pdmath::Rng;
use super::pd_nav::NavGraph;
use super::waypoints::Waypoints;
use super::weapons::{self, WeaponId};

/// The PD bodies the spike can use — the six exported to `assets/enemies/pd/characters`.
pub const BODIES: [&str; 6] = ["pd_joanna", "pd_a51guard", "pd_cassandra", "pd_mrblonde", "pd_ddshock", "pd_elvis"];
const BODY_NAMES: [&str; 6] = ["Joanna", "Guard", "Cassandra", "Mr Blonde", "dataDyne", "Elvis"];
/// `g_HeadsAndBodies[].height` and `.animscale` (`modeldata/robot.c`, NTSC final rows).
const BODY_INFO: [(f32, f32); 6] = [
    (159.0, 0.953_051_6),  // FILE_CDARK_FROCK
    (157.0, 0.927_699_6),  // FILE_CAREA51GUARD
    (167.0, 0.985_915_5),  // FILE_CCASSANDRA
    (169.0, 1.103_286_4),  // FILE_CMRBLONDE
    (157.0, 0.938_967_2),  // FILE_CDDSHOCK
    (106.0, 0.572_769_9),  // FILE_CELVIS1 (a Maian — really that small)
];
const COLORS: [Rgb; 8] = [
    [1.0, 0.35, 0.35],
    [0.35, 0.65, 1.0],
    [0.4, 1.0, 0.45],
    [1.0, 0.85, 0.3],
    [0.85, 0.45, 1.0],
    [0.3, 0.95, 0.95],
    [1.0, 0.6, 0.25],
    [0.9, 0.9, 0.9],
];

/// How long a shot tracer stays on screen, in 60 Hz ticks.
pub const SHOT_LIFETIME: i32 = 12;

/// The N64 frame rate being simulated. PD advances everything by the 240 Hz
/// sub-ticks elapsed since the last frame (`lvupdate240`), so a slower frame rate
/// means bigger, coarser steps — including less frequent sight polling, which is
/// done once per *frame*. Four-player MP on real hardware ran well under 60.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameRate {
    Fps60,
    Fps30,
    Fps20,
    Fps15,
}

impl FrameRate {
    pub const ALL: [FrameRate; 4] = [FrameRate::Fps60, FrameRate::Fps30, FrameRate::Fps20, FrameRate::Fps15];

    pub fn lvupdate240(self) -> u32 {
        match self {
            FrameRate::Fps60 => 4,
            FrameRate::Fps30 => 8,
            FrameRate::Fps20 => 12,
            FrameRate::Fps15 => 16,
        }
    }

    pub fn frame_seconds(self) -> f32 {
        self.lvupdate240() as f32 / 240.0
    }

    pub fn label(self) -> &'static str {
        match self {
            FrameRate::Fps60 => "60 fps (lvupdate240 4)",
            FrameRate::Fps30 => "30 fps (8)",
            FrameRate::Fps20 => "20 fps (12)",
            FrameRate::Fps15 => "15 fps (16)",
        }
    }
}

/// `g_Vars` timing fields (`lv.c:2049-2117`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Globals {
    pub lvupdate240: u32,
    pub lvupdate240rem: u32,
    pub lvupdate60: i32,
    pub lvupdate60f: f32,
    pub lvupdate60freal: f32,
    pub diffframe60: i32,
    pub lvframe60: i32,
}

#[derive(Clone, Debug)]
pub struct Shot {
    pub from: Vec3,
    pub to: Vec3,
    pub hit_chr: Option<usize>,
    pub shooter: usize,
    pub age: i32,
}

/// Which level the match is on (`PD_LEVEL=arena|complex`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelChoice {
    Arena,
    Complex,
}

impl LevelChoice {
    pub fn from_env() -> Self {
        match std::env::var("PD_LEVEL").ok().as_deref() {
            Some(v) if v.eq_ignore_ascii_case("complex") || v.eq_ignore_ascii_case("ref") => LevelChoice::Complex,
            _ => LevelChoice::Arena,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LevelChoice::Arena => "Arena (box room, bots)",
            LevelChoice::Complex => "Complex (PD graph)",
        }
    }
}

/// Which route graph bots use on a PD stage (`PD_NAV=pd|ours`): PD's hand-placed
/// one, or ours generated from the geometry ([`super::navgen`]). Same routing code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavChoice {
    Pd,
    Ours,
}

impl NavChoice {
    pub fn from_env() -> Self {
        match std::env::var("PD_NAV").ok().as_deref() {
            Some(v) if v.eq_ignore_ascii_case("ours") => NavChoice::Ours,
            _ => NavChoice::Pd,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            NavChoice::Pd => "PD's graph",
            NavChoice::Ours => "our generated graph",
        }
    }
}

/// Counters the A/B harness reads.
#[derive(Clone, Debug, Default)]
pub struct SimStats {
    /// `chr_tick_gopos`'s stuck-for-a-second re-routes.
    pub repaths: u32,
    /// `chr_go_to_room_pos` calls, and why the failed ones failed.
    pub gotos: u32,
    pub goto_no_start: u32,
    pub goto_no_end: u32,
    pub goto_no_route: u32,
    /// Where each stuck re-route happened: (chr position, the point it was
    /// heading for).
    pub repath_at: Vec<(Vec3, Vec3)>,
    /// Every `chr_go_to_room_pos` request: the chr's prop position and rooms, and
    /// the destination (for replaying the same requests on another graph).
    pub goto_log: Vec<(Vec3, Vec<u16>, Vec3)>,
}

#[derive(Clone, Debug)]
pub struct SimConfig {
    pub rate: FrameRate,
    pub seed: u64,
    pub bots: Vec<BotConfig>,
    pub waypoint_spacing: f32,
    pub level: LevelChoice,
    pub nav: NavChoice,
}

impl SimConfig {
    /// The default match, overridable from the environment:
    /// `PD_BOTS` (count), `PD_DIFF` (meat..dark), `PD_WEAPON` (a weapon name or
    /// `unarmed`; default is a mixed loadout), `PD_DUAL=1`, `PD_RATE` (60/30/20/15),
    /// `PD_SEED`, `PD_WAYPOINTS` (grid spacing in cm).
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok();
        let n = env("PD_BOTS").and_then(|v| v.parse().ok()).unwrap_or(4usize).clamp(1, 8);
        let difficulty = env("PD_DIFF").and_then(|v| Difficulty::parse(&v)).unwrap_or(Difficulty::Normal);
        let dual = env("PD_DUAL").map_or(false, |v| v == "1");
        let fixed: Option<Option<WeaponId>> = env("PD_WEAPON").map(|v| {
            if v.eq_ignore_ascii_case("unarmed") {
                None
            } else {
                weapons::by_name(&v).map(|w| w.id)
            }
        });
        let mix = [weapons::AR34, weapons::CMP150, weapons::FALCON2, weapons::DRAGON, weapons::K7AVENGER, weapons::DY357];
        let bots = (0..n)
            .map(|k| BotConfig { difficulty, weapon: fixed.unwrap_or(Some(mix[k % mix.len()])), dual })
            .collect();
        let rate = match env("PD_RATE").as_deref() {
            Some("30") => FrameRate::Fps30,
            Some("20") => FrameRate::Fps20,
            Some("15") => FrameRate::Fps15,
            _ => FrameRate::Fps60,
        };
        SimConfig {
            rate,
            seed: env("PD_SEED").and_then(|v| v.parse().ok()).unwrap_or(0xab8d_9f77_8128_0783),
            bots,
            waypoint_spacing: env("PD_WAYPOINTS").and_then(|v| v.parse().ok()).unwrap_or(250.0),
            level: LevelChoice::from_env(),
            nav: NavChoice::from_env(),
        }
    }
}

pub struct Sim {
    /// The box room. Drawn on the arena level, where its [`Arena::geom`] also feeds
    /// `level`; unused on Complex.
    pub arena: Arena,
    /// What the bots collide with, stand on and see through — either level.
    pub level: TileLevel,
    /// Complex's PD data (pads, PD's graph, spawns) when on Complex.
    pub stage: Option<PdStage>,
    /// Run the bot brain (`bot_tick_unpaused`). The walk harness turns it off to
    /// drive a bot itself ([`Sim::walk_straight_to`]).
    pub brains: bool,
    /// The route graph in PD's format: PD's own on Complex, a grid in the arena.
    pub nav: NavGraph,
    /// On a PD stage, the graph not in use (for the viewer's overlays).
    pub other_nav: Option<NavGraph>,
    pub stats: SimStats,
    /// Spawn pads: position + look angle.
    pub spawn_pads: Vec<(Vec3, f32)>,
    pub chrs: Vec<Chr>,
    pub g: Globals,
    pub rng: Rng,
    pub shots: Vec<Shot>,
    pub config: SimConfig,
    pub frame_count: u64,
    pub feed: Vec<(i32, String)>,
}

impl Sim {
    /// A match on the configured level. Panics if Complex can't be loaded; see
    /// [`Sim::try_new`].
    pub fn new(config: SimConfig) -> Self {
        Self::try_new(config).unwrap_or_else(|e| panic!("pd_spike: {e}"))
    }

    pub fn try_new(config: SimConfig) -> Result<Self, String> {
        let arena = Arena::standard();
        let (level, stage, nav, other_nav, spawn_pads) = match config.level {
            LevelChoice::Arena => {
                let level = TileLevel::new(arena.geom());
                let nav = Waypoints::grid(&arena, config.waypoint_spacing).to_nav(&level);
                let spawn_pads = spawn_pads(&arena);
                (level, None, nav, None, spawn_pads)
            }
            LevelChoice::Complex => {
                let stage = PdStage::complex()?;
                let level = TileLevel::new(stage.geom.clone());
                let spawn_pads = stage
                    .spawn_pads
                    .iter()
                    .map(|&p| (stage.pads.pads[p].pos, stage.pads.pads[p].look_angle()))
                    .collect();
                let pd = NavGraph::from_pd_stage(&stage, &level);
                let params = super::navgen::GenParams { crouch_zones: std::env::var("NAVGEN_NO_CROUCH").is_err(), ..Default::default() };
                let ours = super::navgen::generate(&level, &params).graph;
                let (nav, other) = match config.nav {
                    NavChoice::Pd => (pd, ours),
                    NavChoice::Ours => (ours, pd),
                };
                (level, Some(stage), nav, Some(other), spawn_pads)
            }
        };
        let mut sim = Sim {
            arena,
            level,
            stage,
            brains: true,
            nav,
            other_nav,
            stats: SimStats::default(),
            spawn_pads,
            chrs: Vec::new(),
            g: Globals::default(),
            rng: Rng::new(config.seed),
            shots: Vec::new(),
            config,
            frame_count: 0,
            feed: Vec::new(),
        };
        sim.reset(sim.config.bots.len());
        Ok(sim)
    }

    /// SCRIPTED WALKER (spike harness, not PD): a go-to straight at `pos` with no
    /// route, so arrival (`pos_is_arriving_laterally_at_pos`, 30 cm) stops the chr.
    /// Movement, collision and ground are the real bot code; if the chr is stuck
    /// for a second, `chr_tick_gopos` re-routes, finds no graph, and stops it.
    pub fn walk_straight_to(&mut self, i: usize, pos: Vec3) {
        let Some(c) = self.chrs.get_mut(i) else { return };
        if c.is_dead() {
            return;
        }
        c.actiontype = Act::GoPos;
        c.act_gopos = GoPos { endpos: pos, init: true, ..GoPos::default() };
        c.lastmoveok60 = self.g.lvframe60;
    }

    /// Put chr `i` standing exactly on the floor point `floor` (a generated node,
    /// which is a verified standing spot already).
    pub fn place_exact(&mut self, i: usize, floor: Vec3) {
        let c = &mut self.chrs[i];
        c.pos = floor;
        c.prevpos = floor;
        c.ground = floor.y;
        c.sumground = floor.y * 9.999_998;
        c.fallspeed = Vec3::ZERO;
        c.actiontype = Act::Stand;
        c.aibot.moveratex = 0.0;
        c.aibot.moveratey = 0.0;
        c.lastmoveok60 = self.g.lvframe60;
    }

    /// Put chr `i` at a pad the way a spawn would: `chr_adjust_pos_for_spawn` finds
    /// a clear spot at or around it, then the chr stands on that spot's floor.
    pub fn place(&mut self, i: usize, pos: Vec3) {
        let p = self.spawn_spot(i, pos, 0.0);
        let c = &mut self.chrs[i];
        c.pos = p;
        c.prevpos = p;
        c.ground = p.y;
        c.sumground = p.y * 9.999_998;
        c.fallspeed = Vec3::ZERO;
        c.actiontype = Act::Stand;
        c.aibot.moveratex = 0.0;
        c.aibot.moveratey = 0.0;
        c.lastmoveok60 = self.g.lvframe60;
    }

    /// Restart the match with `n` bots (keeping each slot's configuration).
    pub fn reset(&mut self, n: usize) {
        let n = n.clamp(1, 8);
        while self.config.bots.len() < n {
            let last = *self.config.bots.last().unwrap();
            self.config.bots.push(last);
        }
        self.config.bots.truncate(n);
        self.g = Globals::default();
        self.rng = Rng::new(self.config.seed);
        self.shots.clear();
        self.feed.clear();
        self.stats = SimStats::default();
        self.frame_count = 0;
        self.chrs = (0..n)
            .map(|k| {
                let body = k % BODIES.len();
                let (bodyheight, animscale) = BODY_INFO[body];
                Chr {
                    name: format!("{} {}", BODY_NAMES[body], k + 1),
                    body,
                    color: COLORS[k % COLORS.len()],
                    bodyheight,
                    animscale,
                    pos: Vec3::ZERO,
                    prevpos: Vec3::ZERO,
                    radius: 20.0,
                    height: 185.0,
                    actiontype: Act::Stand,
                    act_gopos: GoPos::default(),
                    myaction: MyAction::MainLoop,
                    fadetimer60: 0,
                    fadealpha: -1.0,
                    lastmoveok60: 0,
                    invalidmove: 0,
                    ground: 0.0,
                    sumground: 0.0,
                    fallspeed: Vec3::ZERO,
                    floorroom: None,
                    onladder: false,
                    target: None,
                    damage: 0.0,
                    // `set_chr_maxdamage(CHR_SELF, 80)` × 0.1 (`gailists.c:6059`).
                    maxdamage: 8.0,
                    flinchcnt: -1,
                    flinchtype: 0,
                    aimendlshoulder: 0.0,
                    aimendrshoulder: 0.0,
                    aimendback: 0.0,
                    aimendsideback: 0.0,
                    aimuplshoulder: 0.0,
                    aimuprshoulder: 0.0,
                    aimupback: 0.0,
                    aimsideback: 0.0,
                    aimendcount: 0,
                    model: Anim::default(),
                    weapons_held: [None; 2],
                    hand_firing: [false; 2],
                    gunfire_visible: [false; 2],
                    firecount: [0; 2],
                    unk32c_12: 0,
                    gunpos_rendered: [None; 2],
                    aibot: Aibot::new(self.config.bots[k], n),
                    kills: 0,
                    deaths: 0,
                    suicides: 0,
                    last_choice: None,
                    last_dist: 0.0,
                }
            })
            .collect();
        for k in 0..n {
            bot::bot_spawn(self, k);
            self.chrs[k].aibot.fadeintimer60 = 0;
        }
    }

    /// Change one bot's difficulty / weapon; takes effect at once (and on respawn).
    pub fn set_bot_config(&mut self, i: usize, difficulty: Difficulty, weapon: Option<WeaponId>) {
        let Some(c) = self.chrs.get_mut(i) else { return };
        c.aibot.config.difficulty = difficulty;
        let changed = c.aibot.config.weapon != weapon;
        c.aibot.config.weapon = weapon;
        self.config.bots[i] = c.aibot.config;
        if changed && !c.is_dead() {
            c.weapons_held = [None; 2];
            c.aibot.loadedammo = [0; 2];
            c.aibot.timeuntilreload60 = [0; 2];
            c.aibot.burstsdone = [0; 2];
            c.aibot.nextbullettimer60 = [0; 2];
            bot::give_loadout(c);
            c.aibot.distmode = None;
        }
    }

    /// One N64 frame: PD's timing update (`lv.c`), then every bot's `bot_tick`.
    pub fn frame(&mut self) {
        let lvupdate240 = self.config.rate.lvupdate240();
        let total = lvupdate240 + self.g.lvupdate240rem;
        self.g.lvupdate240 = lvupdate240;
        self.g.lvupdate60 = (total >> 2) as i32;
        self.g.lvupdate240rem = total & 3;
        self.g.lvupdate60f = lvupdate240 as f32 * 0.25;
        self.g.lvupdate60freal = self.g.lvupdate60f;
        self.g.diffframe60 = self.g.lvupdate60;
        self.g.lvframe60 += self.g.lvupdate60;
        self.frame_count += 1;

        let dt = self.g.lvupdate60;
        for s in &mut self.shots {
            s.age += dt;
        }
        self.shots.retain(|s| s.age < SHOT_LIFETIME);

        for i in 0..self.chrs.len() {
            bot::bot_tick(self, i);
        }
        // With the brain off nothing kills or respawns a walker that has fallen
        // out of the world; hold it where it is instead of letting it fall forever.
        if !self.brains {
            for c in &mut self.chrs {
                if c.pos.y < -10_000.0 {
                    c.pos = c.prevpos;
                    c.fallspeed = Vec3::ZERO;
                }
            }
        }
    }

    pub fn log(&mut self, line: String) {
        self.feed.push((self.g.lvframe60, line));
        if self.feed.len() > 8 {
            self.feed.remove(0);
        }
    }

    /// `player_choose_spawn_location` (`player.c:225`), the free-for-all bot case.
    ///
    /// Each pad gets the squared distance to its nearest enemy (every other chr),
    /// and is "very bad" if an enemy is in the pad's room, "bad" if also one is in
    /// a neighbouring room. A 4-slot shortlist then fills in three passes: pads over
    /// 10 m from everyone and not bad (walking circularly from a random pad), then
    /// the same but allowing bad, then whatever is left, furthest first, until the
    /// best left is within 2 m. Every candidate must pass `chr_adjust_pos_for_spawn`.
    /// One shortlisted spot is picked at random; with none, a random pad.
    /// In the one-room arena every pad is "very bad", so only the third pass takes.
    pub fn choose_spawn_location(&mut self, i: usize) -> (Vec3, f32) {
        let numpads = self.spawn_pads.len();
        let mut padsqdists = vec![u32::MAX as f32; numpads];
        let mut verybad = vec![false; numpads];
        let mut bad = vec![false; numpads];
        for (p, &(pad, _)) in self.spawn_pads.iter().enumerate() {
            let padroom = self.level.floor_room(pad, 20.0);
            let neighbours = padroom.map(|r| self.level.room_neighbours(r)).unwrap_or_default();
            for j in 0..self.chrs.len() {
                if j == i {
                    continue;
                }
                let sq = self.chrs[j].prop_pos().distance_squared(pad);
                padsqdists[p] = padsqdists[p].min(sq);
                let rooms = super::chraction::chr_rooms(self, j);
                if padroom.map_or(false, |r| rooms.contains(&r)) {
                    verybad[p] = true;
                }
                if verybad[p] || rooms.iter().any(|r| neighbours.contains(r)) {
                    bad[p] = true;
                }
            }
        }
        let mut shortlist: Vec<(usize, Vec3)> = Vec::new();
        let cyls = super::chraction::perims_except(self, i);
        let adjust = |sim: &Sim, p: usize| {
            let (pad, angle) = sim.spawn_pads[p];
            super::chraction::chr_adjust_pos_for_spawn(&sim.level, 20.0, pad, angle, &cyls)
        };
        // Passes 1 and 2: circular from a random pad, > 10 m, not bad / not very bad.
        for pass in 0..2 {
            let start = (self.rng.random() as usize) % numpads;
            let mut p = start;
            while shortlist.len() < 4 {
                let excluded = if pass == 0 { bad[p] } else { verybad[p] };
                if padsqdists[p] > 1000.0 * 1000.0 && !excluded {
                    if let Some(spot) = adjust(self, p) {
                        shortlist.push((p, spot));
                    }
                    padsqdists[p] = -1.0;
                }
                p = (p + 1) % numpads;
                if p == start {
                    break;
                }
            }
        }
        // Pass 3: whatever is left, furthest first, until the best is within 2 m.
        while shortlist.len() < 4 {
            let mut best: Option<(usize, f32)> = None;
            for (p, &d) in padsqdists.iter().enumerate() {
                if d > best.map_or(-1.0, |(_, bd)| bd) {
                    best = Some((p, d));
                }
            }
            let Some((p, d)) = best else { break };
            if !(d > 200.0 * 200.0) && !shortlist.is_empty() {
                break;
            }
            if let Some(spot) = adjust(self, p) {
                shortlist.push((p, spot));
            }
            padsqdists[p] = -1.0;
        }
        let (pos, angle) = if shortlist.is_empty() {
            self.spawn_pads[(self.rng.random() as usize) % numpads]
        } else {
            let (p, spot) = shortlist[(self.rng.random() as usize) % shortlist.len()];
            (spot, self.spawn_pads[p].1)
        };
        // Stand on the spot's floor (`CHRCFLAG_FORCETOGROUND`).
        (drop_to_ground(&self.level, pos), angle)
    }

    /// Where chr `i` would stand if spawned at a pad: `chr_adjust_pos_for_spawn`,
    /// then the floor under the spot (`CHRCFLAG_FORCETOGROUND`); the pad itself,
    /// dropped, if nowhere around it is clear.
    pub fn spawn_spot(&self, i: usize, pad: Vec3, angle: f32) -> Vec3 {
        let cyls = super::chraction::perims_except(self, i);
        let spot = super::chraction::chr_adjust_pos_for_spawn(&self.level, 20.0, pad, angle, &cyls).unwrap_or(pad);
        drop_to_ground(&self.level, spot)
    }
}

/// A pad or clicked point → where a chr would stand: the floor PD's ground probe
/// finds under a chr cylinder there (pads sit 50–120 cm above their floor).
pub fn drop_to_ground(level: &TileLevel, p: Vec3) -> Vec3 {
    let (g, _) = level.cd_find_ground_at_cyl(Vec3::new(p.x, p.y + 10.0, p.z), 20.0);
    Vec3::new(p.x, if g > -100_000.0 { g } else { p.y }, p.z)
}

/// Eight pads round the room — corners and wall midpoints, 1.2 m in from the
/// walls — each looking at the centre.
fn spawn_pads(arena: &Arena) -> Vec<(Vec3, f32)> {
    let inset = 120.0;
    let (min, max) = (arena.bounds.min + Vec2::splat(inset), arena.bounds.max - Vec2::splat(inset));
    let mid = (min + max) * 0.5;
    [
        (min.x, min.y),
        (mid.x, min.y),
        (max.x, min.y),
        (max.x, mid.y),
        (max.x, max.y),
        (mid.x, max.y),
        (min.x, max.y),
        (min.x, mid.y),
    ]
    .into_iter()
    .filter(|&(x, z)| arena.is_clear(Vec2::new(x, z), 30.0))
    .map(|(x, z)| {
        let look = (-x).atan2(-z);
        (Vec3::new(x, 0.0, z), if look < 0.0 { look + super::pdmath::turn() } else { look })
    })
    .collect()
}
