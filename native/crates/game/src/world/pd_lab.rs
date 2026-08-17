//! **PD simulant lab** — `PD_LAB=1`, a spike that runs our hunters on Perfect
//! Dark's bot model instead of ours, in a bare room, so the difference can be
//! judged by eye.
//!
//! The model itself lives in [`crate::pdsim`] and knows nothing about this game.
//! This module is the seam: it decides which hunters are simulants, feeds the
//! model world state each step, and applies its two outputs — **where the weapon
//! points** and **whether to pull the trigger**.
//!
//! # What changes when a hunter is a simulant
//!
//! | | Our hunter | PD simulant |
//! |---|---|---|
//! | Aim | body faces the AI heading, eased at a fixed rate | body yaw is the *model's* yaw, including its live aim error |
//! | Shot | `rand() < accuracy * (1 - dist/range)` | real hitscan down the barrel, no roll |
//! | Fire gate | FSM entered `Attack` | reaction served + target within 45° of the barrel |
//! | Reaction | one `AiTuning::alert` constant | `shootdelaytimer` that decays rather than resets |
//! | Speed | difficulty multiplier | difficulty tier, or a personality override |
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

/// How the lab was configured at boot.
#[derive(Clone, Copy, Debug)]
pub struct PdLabConfig {
    /// How many simulants to spawn.
    pub count: usize,
    /// Fixed tier, or `None` to follow the live difficulty dial.
    pub difficulty: Option<BotDifficulty>,
    /// Personality applied to every simulant.
    pub bot_type: BotType,
}

impl Default for PdLabConfig {
    /// **Duel mode** — one simulant. This is the *programmatic* default, kept at 1
    /// because the headless scenarios that build a lab want a single bot in isolation.
    /// The environment path ([`Self::from_env`]) deliberately does **not** use this
    /// count; see there for why.
    fn default() -> Self {
        Self { count: 1, difficulty: None, bot_type: BotType::General }
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

/// The tier the lab boots at.
///
/// **Normal, not Dark.** A DarkSim owes no reaction delay and never mis-aims, so it
/// kills on sight from across the arena — which shows the top of the difficulty table
/// and nothing else. Normal has a 0.5 s reaction and wanders ~8°, so the zeroing model
/// is actually visible as behaviour: it swings past you, corrects, and closes.
pub(crate) const LAB_TIER: BotDifficulty = BotDifficulty::Normal;

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

/// The candidate list a simulant at `self_target` chooses from — everyone alive
/// except itself. Weapon score is uniform: our hunters and the player all carry a
/// gun, and PD's threat comparison is about *who is dangerous*, not which gun.
pub(crate) fn candidates(self_target: PdTarget, actors: &[PdActor]) -> (Vec<Candidate>, Vec<PdTarget>) {
    let mut cands = Vec::with_capacity(actors.len());
    let mut who = Vec::with_capacity(actors.len());
    for a in actors {
        if a.who == self_target {
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn step_simulant(
    sim: &mut Simulant,
    dt: f32,
    self_target: PdTarget,
    self_pos: Vec3,
    actors: &[PdActor],
    dial_frac: f32,
    follow_dial: bool,
) -> (SimOutput, PdDebug, Option<PdTarget>) {
    if follow_dial {
        sim.difficulty = tier_for_dial(dial_frac, 1.0);
    }

    // Everyone this simulant could shoot at, with the distance from where it is
    // standing. `candidates` cannot fill distance itself — it does not know whose
    // list it is building.
    let (mut candidates, who) = candidates(self_target, actors);
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
        state: crate::enemy::AiState::Idle,
    };
    (out, debug, chosen)
}
