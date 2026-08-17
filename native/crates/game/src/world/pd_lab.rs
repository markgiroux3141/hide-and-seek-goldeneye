//! **The Perfect Dark hunter model** — how every hunter in this game aims and shoots.
//!
//! This started as `PD_LAB=1`, a spike that ran our hunters on Perfect Dark's bot model
//! in a bare room so the difference could be judged by eye. It won that comparison and
//! **graduated**: [`PdHunters`] is on every `World`, every hunter carries a
//! [`Simulant`], and `PD_LAB` now means only "the bare test room plus the debug
//! overlay". See `DESIGN_PD_SIMULANT_AI.md` §17 for what the promotion moved and what it
//! retired.
//!
//! The model itself lives in [`crate::pdsim`] and knows nothing about this game.
//! This module is the seam: it feeds the model world state each step and applies its two
//! outputs — **where the weapon points** and **whether to pull the trigger**.
//!
//! # What the model replaced
//!
//! The right-hand column is the game today; the left is what it was before the
//! promotion, and none of it is reachable any more.
//!
//! | | The old hunter | Every hunter now |
//! |---|---|---|
//! | Aim | body faces the AI heading, eased at a fixed rate | body yaw is the *model's* yaw, including its live aim error |
//! | Shot | `rand() < accuracy * (1 - dist/range)` | real hitscan down the barrel, no roll |
//! | Fire gate | FSM entered `Attack` | reaction served + target within 45° of the barrel |
//! | Reaction | one `AiTuning::alert` constant | `shootdelaytimer` that decays rather than resets |
//! | Speed | difficulty multiplier | difficulty tier, or a personality override |
//! | Lethality ceiling | `MAX_HIT_RATE`, a global 4 hits/s cap | PD's burst gap, on the cadence |
//! | Body + clips | GoldenEye | Perfect Dark, with the directional fire table |
//!
//! # What deliberately does not change
//!
//! Movement, pathing, local avoidance, cover selection, animation and foot IK all
//! stay on our existing stack. PD's movement layer is built on hand-authored pads
//! and waypoint lists that our levels do not have, and swapping it in would
//! regress the ORCA / nav / foot-IK work for no gain — the thing that reads as "a
//! PD simulant" in a firefight is the aim and the engagement timing, which is
//! exactly what this module replaces.
//!
//! So: the existing FSM still decides *where the hunter goes*. The simulant
//! decides *where it looks and when it shoots*.

use glam::Vec3;

use crate::pdsim::difficulty::{tier_for_dial, BotDifficulty};
use crate::pdsim::personality::{BotType, Candidate, Threat};
use crate::pdsim::{SimInput, SimOutput, Simulant};

/// **How every hunter fights.** Perfect Dark's bot model, and the game's AI rather than
/// a spike: this is present on every `World` whether or not the lab is on.
///
/// The lab ([`PdLabConfig`]) still exists and still overrides these knobs, but it now
/// configures the *room and the instrumentation* around the model — a bare box, a pinned
/// tier, the debug overlay — not whether the model runs.
#[derive(Clone, Copy, Debug)]
pub struct PdHunters {
    /// Fixed tier, or `None` to follow the live difficulty dial (the normal game).
    pub difficulty: Option<BotDifficulty>,
    /// Personality applied to every hunter.
    ///
    /// `General` in the normal game, deliberately. Half of [`BotType`] would stop a
    /// hunter hunting — `Peace` never fires, `Coward` flees unless it out-guns you — so
    /// the varied-personality squad is a lab toy (`PD_LAB_TYPE`) until personalities are
    /// picked per hunter rather than per wave.
    pub bot_type: BotType,
    /// Teams off — see [`PdLabConfig::free_for_all`]. Off in the normal game: the
    /// hunters are a squad.
    pub free_for_all: bool,
}

impl Default for PdHunters {
    fn default() -> Self {
        Self { difficulty: None, bot_type: BotType::General, free_for_all: false }
    }
}

impl From<PdLabConfig> for PdHunters {
    fn from(c: PdLabConfig) -> Self {
        Self { difficulty: c.difficulty, bot_type: c.bot_type, free_for_all: c.free_for_all }
    }
}

/// How the lab was configured at boot.
#[derive(Clone, Copy, Debug)]
pub struct PdLabConfig {
    /// How many simulants to spawn.
    pub count: usize,
    /// Fixed tier, or `None` to follow the live difficulty dial.
    pub difficulty: Option<BotDifficulty>,
    /// Personality applied to every simulant.
    pub bot_type: BotType,
    /// **Teams off** — every character is every other character's enemy, so the pack
    /// fights itself as well as the player. This is `MPOPTION_TEAMSENABLED` inverted:
    /// PD's free-for-all is not a separate mode, it is a team check that passes for
    /// everybody (`chr_compare_teams`, `chraction.c:14880`).
    ///
    /// **Off by default**, because the hunters are a squad. With it on, a hunter's
    /// nearest packmate is nearly always the closest thing it can see, so
    /// `bot_choose_general_target`'s ascending-distance walk picks a packmate over the
    /// player almost every time — and now that the FSM manoeuvres against its target
    /// (§14), the pack stops coming for the player at all. See [`is_friend`].
    pub free_for_all: bool,
}

impl Default for PdLabConfig {
    /// **Duel mode** — one simulant. This is the *programmatic* default, kept at 1
    /// because the headless scenarios that build a lab want a single bot in isolation.
    /// The environment path ([`Self::from_env`]) deliberately does **not** use this
    /// count; see there for why.
    fn default() -> Self {
        Self { count: 1, difficulty: None, bot_type: BotType::General, free_for_all: false }
    }
}

impl PdLabConfig {
    /// Parse the lab configuration from the environment.
    ///
    /// * `PD_LAB=1` — on, following the dial, with a pack the size of the normal
    ///   game's ([`crate::world::PLAYTEST_WAVE_SIZE`]).
    /// * `PD_LAB_COUNT=n` — spawn `n` simulants instead (1 = duel mode).
    /// * `PD_LAB_DIFFICULTY=meat|easy|normal|hard|perfect|dark` — pin a tier
    ///   rather than following the `=` / `-` dial.
    /// * `PD_LAB_TYPE=general|peace|coward|prey|feud|kaze|speed|turtle|…` — the
    ///   personality axis.
    /// * `PD_LAB_FFA=1` — **teams off**: the hunters fight each other as well as the
    ///   player. Off by default; see [`Self::free_for_all`].
    ///
    /// Returns `None` when `PD_LAB` is unset, which is the normal game.
    ///
    /// **Why the count doesn't come from [`Default`].** `World::enable_pd_lab` sets the
    /// wave size from this config, and it runs *after* the app has already pinned
    /// [`crate::world::PLAYTEST_WAVE_SIZE`] — so a `default()` of 1 meant that merely
    /// turning the lab on silently cut the wave from four hunters to one, with nothing
    /// on screen or in the log to say so. That was defensible when the lab was one bot
    /// in a bare room; it is actively wrong now that it is used to watch a *pack*
    /// navigate a real level. Turning the lab on should change which AI the hunters
    /// run, not how many of them there are.
    pub fn from_env() -> Option<Self> {
        std::env::var("PD_LAB").ok()?;
        let mut cfg = PdLabConfig { count: crate::world::PLAYTEST_WAVE_SIZE, ..Default::default() };
        if let Some(n) = std::env::var("PD_LAB_COUNT").ok().and_then(|s| s.trim().parse().ok()) {
            cfg.count = n;
        }
        if let Ok(d) = std::env::var("PD_LAB_DIFFICULTY") {
            cfg.difficulty = parse_difficulty(&d);
            if cfg.difficulty.is_none() {
                log::warn!("PD_LAB_DIFFICULTY='{d}' not recognised — following the dial");
            }
        }
        cfg.free_for_all = std::env::var("PD_LAB_FFA").is_ok();
        if let Ok(t) = std::env::var("PD_LAB_TYPE") {
            match parse_bot_type(&t) {
                Some(ty) => cfg.bot_type = ty,
                None => log::warn!("PD_LAB_TYPE='{t}' not recognised — using GeneralSim"),
            }
        }
        Some(cfg)
    }
}

// ─── Character showcase (retired) ────────────────────────────────────────────
//
// `PdShowcase` stood the six converted PD bodies in a row in the lab, each looping
// one locomotion clip, as the animated successor to the static
// `PropCategory::PerfectDark` preview props. It existed for one reason: hunters
// could not wear a PD body, so a lineup was the only way to see one move.
//
// Hunters wear them now (`World::spawn_family`), which is a strictly better
// showcase — the bodies are seen fighting, taking hits and dying, on the same code
// path as everything else, instead of miming in a corner. It is deleted rather than
// kept behind a flag because it was also never seen: it placed its figures at
// x = 5.0..13.0 m in a room that is 4.5 m across, so every one of them stood
// outside the level, embedded in the wall. Nothing rendered, and nothing said so.

/// The tier a normalised (0..1) difficulty-dial position maps onto.
pub(crate) fn tier_for_dial_frac(frac: f32) -> BotDifficulty {
    tier_for_dial(frac, 1.0)
}

/// **The tier the game boots at**, lab or not.
///
/// **Normal, not Dark.** A DarkSim owes no reaction delay and never mis-aims, so it kills
/// on sight from across the room — which shows the top of the difficulty table and
/// nothing else. Normal has a 0.5 s reaction and wanders ~8°, so the zeroing model is
/// actually visible as behaviour: it swings past you, corrects, and closes.
///
/// This mattered only to the lab while the hit roll was still the game's shot model,
/// because the dial's own boot position was `DIFFICULTY_MAX` and max meant "1.6× accuracy
/// on a probability". Now that the roll is retired (§17), max means **DarkSim** — perfect
/// aim, no reaction — so booting the real game there would make it unplayable rather than
/// hard. The `=` / `-` keys still sweep the whole table live.
pub(crate) const HUNT_TIER: BotDifficulty = BotDifficulty::Normal;

/// The lowest dial position (0..=`max`) whose tier is `want` — so the lab can boot at
/// a named tier while the `=` / `-` keys keep working, rather than pinning the tier
/// and disconnecting the dial. Derived by asking [`tier_for_dial`] rather than
/// hard-coding a number, so re-ordering the tier table cannot silently mis-set it.
pub(crate) fn dial_for_tier(want: BotDifficulty, max: u32) -> u32 {
    (0..=max).find(|d| tier_for_dial(*d as f32, max as f32) == want).unwrap_or(max / 2)
}

fn parse_difficulty(s: &str) -> Option<BotDifficulty> {
    let s = s.trim().to_ascii_lowercase();
    BotDifficulty::ALL
        .iter()
        .copied()
        .find(|d| d.name().to_ascii_lowercase().trim_end_matches("sim") == s)
}

fn parse_bot_type(s: &str) -> Option<BotType> {
    let s = s.trim().to_ascii_lowercase();
    BotType::ALL
        .iter()
        .copied()
        .find(|t| t.name().to_ascii_lowercase().trim_end_matches("sim") == s)
}

/// Everything the debug overlay shows for one simulant. Snapshotted each frame so
/// the UI never borrows the world.
#[derive(Clone, Copy, Debug)]
pub struct PdDebug {
    pub tier: BotDifficulty,
    pub bot_type: BotType,
    /// Zero convergence, 0..1.
    pub zero_progress: f32,
    /// Live aim error in degrees, signed.
    pub aim_error_deg: f32,
    /// Reaction clock (s) against the tier's requirement.
    pub shoot_delay: f32,
    pub shoot_delay_needed: f32,
    /// Whether the model wants to fire this frame.
    pub firing: bool,
    /// Whether it has a target at all, and whether it kept it without re-deciding.
    pub has_target: bool,
    pub sticky: bool,
    /// Distance to the player (m).
    pub distance: f32,
    /// Movement speed multiplier in force.
    pub speed_mult: f32,
    /// Remaining / spawn health, and whether this one has been killed. Filled in by
    /// the caller rather than the model: the [`Simulant`] has no notion of a body.
    /// A dead simulant keeps reporting (the overlay shows it as DEAD rather than
    /// silently dropping the row), so the roster's indices stay stable across a fight.
    pub health: f32,
    pub max_health: f32,
    pub dead: bool,
    /// The hunter's index in the roster, so the overlay and the radar agree on who
    /// `#3` is even once some of them are corpses.
    pub id: usize,
    /// **Who it is manoeuvring against** — the free-for-all readout. `None` means the
    /// player (or nothing picked yet); `Some(j)` is packmate `#j`. Filled in by the
    /// caller from [`PdTarget`].
    ///
    /// With teams on (the default) this is always `None`, and that is the point: a
    /// packmate showing up here means either `PD_LAB_FFA=1` or a broken team check.
    pub target_hunter: Option<usize>,
    /// Whether teams are off ([`PdLabConfig::free_for_all`]) — shown in the overlay so
    /// "they are ignoring me and fighting each other" can be read as the mode rather
    /// than as a bug.
    pub free_for_all: bool,
    /// Which attack animation the **current burst** is playing (`ANIM_xxxx`), and how far
    /// off the body's facing it aims — the direction-table readout. `None` between
    /// bursts. A non-zero angle means the body is deliberately turned away from the
    /// target; see `combat::attack_anim`.
    pub fire_anim: Option<(&'static str, f32)>,
    /// What this hunter's FSM is doing — the readout for "why is it not moving".
    pub state: crate::enemy::AiState,
}

/// One character a simulant may target: the player, or another hunter.
///
/// The lab's candidate list used to be the player alone, which left the half of
/// [`BotType`] that compares *between* candidates — Prey, Judge, Venge, Feud, Coward
/// — with nothing to compare. Every live hunter is a candidate now, so those vetoes
/// run against real alternatives.
///
/// Ids are stable within a step: `0` is the player and hunter `i` is `i + 1`, which
/// is what [`Simulant::target`](crate::pdsim::Simulant::target) and the grudge memory
/// hold onto between ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PdTarget {
    Player,
    Hunter(usize),
}

impl PdTarget {
    /// The stable candidate id for this target.
    pub(crate) fn id(self) -> u32 {
        match self {
            PdTarget::Player => 0,
            PdTarget::Hunter(i) => i as u32 + 1,
        }
    }

    fn from_id(id: u32) -> PdTarget {
        match id {
            0 => PdTarget::Player,
            n => PdTarget::Hunter(n as usize - 1),
        }
    }
}

/// A shootable character, snapshotted before the hunter loop so a simulant can be
/// told about its neighbours without borrowing the roster it is inside.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PdActor {
    pub who: PdTarget,
    pub pos: Vec3,
    pub alive: bool,
    pub health_frac: f32,
    pub armed: bool,
    /// Whether this simulant can see it right now. Only the one candidate the
    /// round-robin asks about is freshly raycast; the rest carry the last answer.
    pub visible: bool,
}

/// Whether two characters are on the same side, given whether teams are enabled —
/// `chr_compare_teams(.., COMPARE_FRIENDS)` (`chraction.c:14860`).
///
/// The hunters are one team and the player is the other, which is the shape PD's
/// campaign has (`chr->team` versus Bond) and its team multiplayer has
/// (`MPOPTION_TEAMSENABLED`). With `free_for_all` the check passes for nobody, which is
/// exactly how PD implements free-for-all: `COMPARE_ENEMIES` returns true whenever
/// `(g_MpSetup.options & MPOPTION_TEAMSENABLED) == 0`, regardless of team.
pub(crate) fn is_friend(a: PdTarget, b: PdTarget, free_for_all: bool) -> bool {
    if free_for_all {
        return false;
    }
    matches!((a, b), (PdTarget::Hunter(_), PdTarget::Hunter(_)))
}

/// The candidate list a simulant at `self_target` chooses from — every alive **enemy**.
/// Weapon score is uniform: our hunters and the player all carry a gun, and PD's threat
/// comparison is about *who is dangerous*, not which gun.
///
/// Teammates are excluded here rather than vetoed later, because
/// `bot_choose_general_target` does the same thing in the same place: its
/// ascending-distance walk skips any chr failing
/// `chr_compare_teams(botchr, trychr, COMPARE_ENEMIES)`, and separately drops an
/// existing target that turns out to be a friend. Leaving packmates in the list and
/// hoping distance sorts it out does not work — a packmate is nearly always the closest
/// visible character there is.
pub(crate) fn candidates(
    self_target: PdTarget,
    actors: &[PdActor],
    free_for_all: bool,
) -> (Vec<Candidate>, Vec<PdTarget>) {
    let mut cands = Vec::with_capacity(actors.len());
    let mut who = Vec::with_capacity(actors.len());
    for a in actors {
        if a.who == self_target || is_friend(self_target, a.who, free_for_all) {
            continue;
        }
        cands.push(Candidate {
            id: a.who.id(),
            distance: 0.0, // filled by the caller, which knows its own position
            in_sight: a.visible,
            alive: a.alive,
            threat: Threat {
                weapon_score: if a.armed { 50.0 } else { 0.0 },
                health_frac: a.health_frac,
                since_spawn: 1.0e6,
                score: 0.0,
            },
            armed: a.armed,
        });
        who.push(a.who);
    }
    (cands, who)
}

/// Feed one simulant a step of world state and get its aim + fire decision.
///
/// `bearing` uses the same yaw convention as the rest of the game
/// (`atan2(dx, dz)`, so yaw 0 faces +Z), which is what
/// [`EnemyInstance::yaw`](super::EnemyInstance::yaw) reads.
///
/// `player_visible` is a raw line-of-sight result, deliberately *not* the FSM's
/// cone-gated perception: PD bots run their own sight check independent of the
/// facing-cone logic, and feeding the FSM's answer in would make the aim model
/// inherit a perception rule it was not designed against. The caller does that one
/// raycast per simulant per step — cheap, and the same order of work PD does.
///
/// The round-robin amortisation in [`Perception`](crate::pdsim::targeting::Perception)
/// still governs which candidate the simulant *believes* it can see, which is the
/// behaviourally interesting half.
///
/// `aim_offset` is the playing attack animation's `angleoffset` (0 when not firing, or
/// when the animation aims straight ahead). It biases the body's turn goal rather than
/// the barrel — see [`Simulant::yaw`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn step_simulant(
    sim: &mut Simulant,
    dt: f32,
    self_target: PdTarget,
    self_pos: Vec3,
    actors: &[PdActor],
    dial_frac: f32,
    follow_dial: bool,
    aim_offset: f32,
    free_for_all: bool,
) -> (SimOutput, PdDebug, Option<PdTarget>) {
    if follow_dial {
        sim.difficulty = tier_for_dial(dial_frac, 1.0);
    }

    // Every ENEMY this simulant could shoot at, with the distance from where it is
    // standing. `candidates` cannot fill distance itself — it does not know whose
    // list it is building.
    let (mut candidates, who) = candidates(self_target, actors, free_for_all);
    let pos_of = |t: PdTarget| actors.iter().find(|a| a.who == t).map(|a| a.pos);
    for (c, t) in candidates.iter_mut().zip(&who) {
        if let Some(p) = pos_of(*t) {
            c.distance = Vec3::new(p.x - self_pos.x, 0.0, p.z - self_pos.z).length();
        }
    }

    // Answer exactly the one sight question the round-robin is asking this tick
    // (`Perception::next_query`) — the caller already resolved visibility for every
    // actor, so this just routes the right answer to the right slot. Every other
    // candidate keeps ageing on stale knowledge, which is the point of the model.
    let query = sim
        .perception
        .next_query()
        .filter(|i| *i < candidates.len())
        .map(|i| (i, candidates[i].in_sight, candidates[i].distance));

    // Bearing to the target the simulant is *currently* holding, since target
    // selection happens inside `tick` and may keep it (`sticky`).
    let current = sim.target.map(PdTarget::from_id);
    let (bearing, target_in_sight) = match current.and_then(|t| {
        pos_of(t).map(|p| (t, p))
    }) {
        Some((t, p)) => {
            let flat = Vec3::new(p.x - self_pos.x, 0.0, p.z - self_pos.z);
            let seen = actors.iter().find(|a| a.who == t).is_some_and(|a| a.visible && a.alive);
            let b = if flat.length() > 1e-4 { Some(flat.x.atan2(flat.z)) } else { None };
            (b, seen)
        }
        None => (None, false),
    };

    let out = sim.tick(SimInput {
        dt,
        candidates: &candidates,
        own: Threat::of(50.0, 1.0),
        query,
        bearing_to_target: bearing,
        target_in_sight,
        aim_offset,
        dial_frac,
    });
    let chosen = out.target_index.and_then(|i| who.get(i).copied());
    let distance = out
        .target_index
        .and_then(|i| candidates.get(i))
        .map(|c| c.distance)
        .unwrap_or(0.0);

    let tuning = sim.tuning();
    let debug = PdDebug {
        tier: sim.difficulty,
        bot_type: sim.bot_type,
        zero_progress: out.zero_progress,
        aim_error_deg: out.aim_error.to_degrees(),
        shoot_delay: sim.zero.shoot_delay_timer,
        shoot_delay_needed: tuning.shoot_delay,
        firing: out.want_fire,
        has_target: out.target_index.is_some(),
        sticky: sim.sticky,
        distance,
        speed_mult: out.speed_mult,
        // Body-side fields — the caller owns these (see `PdDebug`), so they are
        // placeholders here and are overwritten in `World::fixed_step`.
        health: 0.0,
        max_health: 0.0,
        dead: false,
        id: 0,
        target_hunter: None,
        free_for_all,
        fire_anim: None,
        state: crate::enemy::AiState::Idle,
    };
    (out, debug, chosen)
}
