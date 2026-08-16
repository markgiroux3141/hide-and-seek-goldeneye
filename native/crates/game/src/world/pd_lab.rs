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

use glam::{Mat4, Vec3};

use engine::skeletal::clip::AnimationClip;

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
    fn default() -> Self {
        Self { count: 1, difficulty: None, bot_type: BotType::General }
    }
}

impl PdLabConfig {
    /// Parse the lab configuration from the environment.
    ///
    /// * `PD_LAB=1` — on, with defaults (one GeneralSim following the dial).
    /// * `PD_LAB_COUNT=n` — spawn `n` simulants instead of one.
    /// * `PD_LAB_DIFFICULTY=meat|easy|normal|hard|perfect|dark` — pin a tier
    ///   rather than following the `=` / `-` dial.
    /// * `PD_LAB_TYPE=general|peace|coward|prey|feud|kaze|speed|turtle|…` — the
    ///   personality axis.
    ///
    /// Returns `None` when `PD_LAB` is unset, which is the normal game.
    pub fn from_env() -> Option<Self> {
        std::env::var("PD_LAB").ok()?;
        let mut cfg = PdLabConfig::default();
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

// ─── Character showcase ──────────────────────────────────────────────────────

/// The Perfect Dark character lineup in the lab: one figure per PD body, each
/// playing a different clip so the whole locomotion set is visible at a glance.
///
/// This **replaces** the four static `PropCategory::PerfectDark` preview props.
/// Those were baked single poses exported to OBJ — they proved the model, skeleton
/// and animation decoders agreed, and could prove nothing more. These are real
/// [`SkinnedModel`](engine::skeletal::gltf_skin::SkinnedModel)s posed every frame
/// through the engine's own skinning path, which is the thing that actually needed
/// proving.
///
/// Kept out of the hunter roster deliberately: a hunter needs fire/hit/death clips
/// that PD's animation set has not been triaged for yet (see
/// [`PD_BODY_CATALOG`](super::PD_BODY_CATALOG)). Nothing here touches the AI.
pub(crate) struct PdShowcase {
    figures: Vec<PdFigure>,
}

struct PdFigure {
    /// Body id into [`World::char_models`](super::World::char_models).
    body: usize,
    /// Index into [`World::pd_clips`](super::World::pd_clips).
    clip: usize,
    feet: Vec3,
    yaw: f32,
    /// Playback clock, wrapped into the clip's duration.
    clock: f32,
}

/// Where the lineup stands: the far side of the `pd_lab` room, spaced so you can
/// walk between them, facing the player's spawn corner. Matches the placement the
/// preview props had.
const LINEUP_Z: f32 = 3.0;
const LINEUP_X0: f32 = 5.0;
const LINEUP_SPACING: f32 = 1.6;

impl PdShowcase {
    /// Build the lineup over the loaded PD bodies. `None` if the PD assets are
    /// missing, so the lab still boots (as it did before they existed).
    pub(crate) fn new(bodies: std::ops::Range<usize>, clip_count: usize) -> Option<Self> {
        if bodies.is_empty() || clip_count == 0 {
            return None;
        }
        let figures = bodies
            .enumerate()
            .map(|(i, body)| PdFigure {
                body,
                // One clip each, cycling — the lineup shows idle/walk/jog/run at once
                // rather than needing four visits.
                clip: i % clip_count,
                feet: Vec3::new(LINEUP_X0 + i as f32 * LINEUP_SPACING, 0.0, LINEUP_Z),
                // Face +Z, toward the player's corner.
                yaw: 0.0,
                // Stagger the clocks so identical clips don't march in lockstep.
                clock: i as f32 * 0.37,
            })
            .collect();
        Some(PdShowcase { figures })
    }

    pub(crate) fn advance(&mut self, dt: f32, clips: &[AnimationClip]) {
        for f in &mut self.figures {
            if let Some(c) = clips.get(f.clip) {
                f.clock = if c.duration > 0.0 { (f.clock + dt) % c.duration } else { 0.0 };
            }
        }
    }

    /// `(body id, feet, yaw, joint matrices)` per figure, for the character draw
    /// pass. Bodies whose clip or model is missing are skipped rather than drawn in
    /// bind pose — a splayed star on screen would read as a rendering bug.
    pub(crate) fn instances<'a>(
        &'a self,
        models: &'a [engine::skeletal::gltf_skin::SkinnedModel],
        clips: &'a [AnimationClip],
    ) -> impl Iterator<Item = (usize, Vec3, f32, Vec<Mat4>)> + 'a {
        self.figures.iter().filter_map(move |f| {
            let m = models.get(f.body)?;
            let c = clips.get(f.clip)?;
            Some((f.body, f.feet, f.yaw, c.skinning_matrices(f.clock, &m.skeleton)))
        })
    }
}

/// The tier a normalised (0..1) difficulty-dial position maps onto.
pub(crate) fn tier_for_dial_frac(frac: f32) -> BotDifficulty {
    tier_for_dial(frac, 1.0)
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
    self_pos: Vec3,
    player_feet: Vec3,
    player_visible: bool,
    player_health_frac: f32,
    player_armed: bool,
    dial_frac: f32,
    follow_dial: bool,
) -> (SimOutput, PdDebug) {
    if follow_dial {
        sim.difficulty = tier_for_dial(dial_frac, 1.0);
    }

    let flat = Vec3::new(player_feet.x - self_pos.x, 0.0, player_feet.z - self_pos.z);
    let distance = flat.length();
    let bearing = if distance > 1e-4 { Some(flat.x.atan2(flat.z)) } else { None };

    // The lab's candidate set is just the player. Multi-simulant deathmatch —
    // where target selection and the personality vetoes really earn their keep —
    // needs enemy-vs-enemy damage, which the game does not have yet.
    let candidates = [Candidate {
        id: 0,
        distance,
        in_sight: player_visible,
        alive: true,
        threat: Threat {
            weapon_score: if player_armed { 50.0 } else { 0.0 },
            health_frac: player_health_frac,
            since_spawn: 1.0e6,
            score: 0.0,
        },
        armed: player_armed,
    }];

    let out = sim.tick(SimInput {
        dt,
        candidates: &candidates,
        own: Threat::of(50.0, 1.0),
        query: Some((0, player_visible, distance)),
        bearing_to_target: bearing,
        target_in_sight: player_visible,
        dial_frac,
    });

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
    };
    (out, debug)
}
