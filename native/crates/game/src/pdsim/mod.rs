//! **Perfect Dark simulant AI** — a faithful port of PD's multiplayer bot model.
//!
//! Reference notes and the reasoning behind each piece are in
//! `DESIGN_PD_SIMULANT_AI.md`; source line references throughout point at
//! `reference/pd-decomp/src/game/` (gitignored — see `reference/README.md`).
//!
//! # What this replaces
//!
//! Our existing hunters resolve a shot by rolling `accuracy * (1 - dist/range)`
//! and applying damage on a win (`world::combat::emit_enemy_shot`). PD does not
//! do that, at all. A simulant points a real weapon in world space and fires a
//! genuine hitscan; whether it connects depends only on where the barrel actually
//! is. Every behaviour players read as "human" — leading badly at low skill,
//! being thrown off by a target that forces a turn, spraying wide for a moment
//! after whipping around a corner — falls out of that one decision.
//!
//! This module owns the *decision and aim* half of a simulant. It does not move
//! anything, does not touch the renderer, and does not know what a hunter is; the
//! caller feeds it world state and gets back an aim yaw and a fire verdict. That
//! keeps it unit-testable against the PD constants and keeps the existing
//! nav/avoidance/animation stack untouched.
//!
//! # Structure
//!
//! Difficulty and personality are **orthogonal axes**, exactly as in PD:
//!
//! * [`difficulty`] scales lethality only — reaction, aim convergence, speed.
//! * [`personality`] changes target selection and goals only — never accuracy.
//!
//! [`zeroing`] is the aim model itself, and [`targeting`] is the shared selection
//! algorithm the personalities veto against.
//!
//! [`distmode`] is the one exception to "this module does not move anything": it is
//! PD's combat *movement decision* (`botcmd_tick_dist_mode`), which belongs with the
//! rest of the bot model even though the execution is the hunter's. It is reached
//! only under `AI=pd` — see [`crate::enemy::AiMode`].

pub mod difficulty;
pub mod distmode;
pub mod personality;
pub mod spread;
pub mod targeting;
pub mod zeroing;

#[cfg(test)]
mod tests;

use difficulty::{BotDifficulty, BotTuning};
use personality::{BotType, Candidate, Grudge, Threat};
use targeting::Perception;
use zeroing::Zeroing;

/// The half-angle of the cone within which a bot will pull the trigger
/// (`chr_is_target_in_fov(chr, 45, false)`, `bot.c:3606`).
///
/// This is deliberately generous — 45°, not "when zeroed". PD bots open fire
/// while still converging, which is precisely why they spray wide on first
/// contact and tighten up as the zero completes. Gating fire on a completed zero
/// would produce a much less interesting (and much deadlier) enemy.
pub const FIRE_FOV_HALF_ANGLE: f32 = 45.0 * std::f32::consts::PI / 180.0;

/// One simulant's complete AI state.
#[derive(Clone, Debug)]
pub struct Simulant {
    /// Lethality axis.
    pub difficulty: BotDifficulty,
    /// Behaviour axis.
    pub bot_type: BotType,
    /// Aim convergence.
    pub zero: Zeroing,
    /// Amortised knowledge of other characters.
    pub perception: Perception,
    /// Personality memory (feud nemesis, last killer).
    pub grudge: Grudge,
    /// The id of the currently selected target, if any.
    pub target: Option<u32>,
    /// **Body** yaw (rad) — where the torso faces. The aim error lives in here, not
    /// in a separate accuracy term.
    ///
    /// This is *not* always the direction a shot goes: PD's attack animations each
    /// declare an `angleoffset`, how far their aim-zero sits off the body's facing,
    /// and the body is deliberately turned to `bearing - angleoffset` so the
    /// animation's own aim lands on the target (`chr_turn`'s tolerance argument,
    /// `chraction.c:10758`). [`Self::barrel_yaw`] is what the round follows.
    pub yaw: f32,
    /// The `angleoffset` of the attack animation currently playing (rad, `+` left),
    /// as handed in by [`SimInput::aim_offset`]. `0` whenever the body is aiming
    /// straight down its own facing, which is every case except a sideways clip.
    pub aim_offset: f32,
    /// The angular rate the body turned last step (rad/s), fed back into the
    /// zeroing model so correcting the aim costs zero progress.
    pub turn_rate: f32,
    /// Whether the last [`Self::tick`] chose to keep its target without
    /// re-deciding. Diagnostic only.
    pub sticky: bool,
    /// xorshift RNG state, so a simulant's aim wander is reproducible per seed.
    rng: u64,
}

/// What the caller must supply each tick.
pub struct SimInput<'a> {
    pub dt: f32,
    /// Every character this simulant could target, including the player.
    pub candidates: &'a [Candidate],
    /// The simulant's own threat rating, for the coward comparison.
    pub own: Threat,
    /// Fresh sight/distance for the one candidate the round-robin asked about
    /// (see [`Perception::next_query`]). `None` when there is nothing to query.
    pub query: Option<(usize, bool, f32)>,
    /// True bearing (rad) from the simulant to its current target, if it has one.
    pub bearing_to_target: Option<f32>,
    /// Whether the current target is in sight *right now* (the caller's own
    /// authority, since it owns the physics world).
    pub target_in_sight: bool,
    /// `angleoffset` (rad, `+` left) of the attack animation currently playing, or `0`
    /// when none is. The body turns to `bearing - this`, so a sideways animation keeps
    /// the torso turned away while the barrel still points at the target — PD's
    /// `chr_turn(..., toleranceangle = animcfg->angleoffset)`.
    pub aim_offset: f32,
    /// Position on the 0..=`max_dial` difficulty dial, used only to decide the
    /// oblivious-targeting cutover. The tier itself comes from `difficulty`.
    pub dial_frac: f32,
}

/// What the simulant decided this tick.
#[derive(Clone, Copy, Debug, Default)]
pub struct SimOutput {
    /// Index into the candidate slice of the chosen target.
    pub target_index: Option<usize>,
    /// Body yaw after turning (rad) — what the model is rendered at.
    pub yaw: f32,
    /// Where the **weapon** points after turning (rad) — `yaw + aim_offset`. Fire
    /// along this; it is the same as [`Self::yaw`] unless a sideways attack animation
    /// is playing.
    pub barrel_yaw: f32,
    /// Signed aim error still on the weapon (rad). Zero means dead on.
    pub aim_error: f32,
    /// Zero progress, 0..1, for the debug HUD.
    pub zero_progress: f32,
    /// The bot wants to shoot this tick: reaction served, target in sight and
    /// alive, and the target inside [`FIRE_FOV_HALF_ANGLE`] of the barrel.
    pub want_fire: bool,
    /// Movement speed multiplier against our Normal-tier baseline.
    pub speed_mult: f32,
}

impl Simulant {
    pub fn new(difficulty: BotDifficulty, bot_type: BotType, seed: u64) -> Self {
        Self {
            difficulty,
            bot_type,
            zero: Zeroing::default(),
            perception: Perception::default(),
            grudge: Grudge::default(),
            target: None,
            yaw: 0.0,
            aim_offset: 0.0,
            turn_rate: 0.0,
            sticky: false,
            rng: seed | 1,
        }
    }

    pub fn tuning(&self) -> BotTuning {
        self.difficulty.tuning()
    }

    /// Where the weapon points: the body yaw plus the playing animation's
    /// `angleoffset`. Every shot goes along this, and it equals [`Self::yaw`] unless a
    /// sideways attack animation is running. See [`Self::aim_offset`].
    pub fn barrel_yaw(&self) -> f32 {
        self.yaw + self.aim_offset
    }

    /// Movement speed multiplier (`bot_calculate_max_speed`, `bot.c:1096`).
    ///
    /// Personality *overrides* the difficulty scale rather than multiplying it, so
    /// a SpeedSim moves identically at Meat and at Dark.
    pub fn speed_mult(&self) -> f32 {
        self.bot_type.speed_override().unwrap_or_else(|| self.difficulty.speed_ratio())
    }

    /// xorshift64 → uniform `[0, 1)`. Taken by raw state rather than `&mut self`
    /// so the zeroing update can borrow `self.zero` and the RNG at once.
    fn draw(state: &mut u64) -> f32 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        ((*state >> 40) as f32) / ((1u32 << 24) as f32)
    }

    /// Advance one tick: perceive, choose a target, converge the aim, decide
    /// whether to shoot.
    pub fn tick(&mut self, input: SimInput<'_>) -> SimOutput {
        let tuning = self.tuning();
        let dt = input.dt;

        // ── Perception: exactly one fresh query, everyone else ages ──
        self.perception.resize(input.candidates.len());
        self.perception.tick(dt, input.query);

        // ── Target selection ──
        let previous = self.target;
        let selection = targeting::choose_target(
            input.candidates,
            &self.perception,
            self.bot_type,
            &input.own,
            &mut self.grudge,
            previous,
            targeting::takes_unseen_closest(input.dial_frac),
        );
        self.sticky = selection.sticky;
        let chosen_id = selection.index.map(|i| input.candidates[i].id);
        if chosen_id != previous {
            // A target *change* restarts the reaction delay from scratch
            // (`bot_set_target`, bot.c:1366) — but note it does not reset the zero
            // timer, which only ever drains.
            self.zero.on_target_changed();
        }
        self.target = chosen_id;

        // ── Aim ──
        let in_sight = input.target_in_sight && self.target.is_some();
        self.zero.tick_shoot_delay(dt, in_sight);

        // Feed back the turn made *last* step, matching PD's ordering: the zero
        // update reads `speedtheta`, which `bot_tick` wrote before calling it.
        let turn_rate = self.turn_rate;
        let mut rng = self.rng;
        let aim_error =
            self.zero.update(dt, &tuning, in_sight, turn_rate, false, || Self::draw(&mut rng));
        self.rng = rng;

        // Turn the body toward the bearing plus the error, LESS the playing
        // animation's `angleoffset`. The error is added to the *true* bearing, so the
        // body chases a target that is itself wandering — the bot is never told where
        // the mistake is. Subtracting the offset is PD's turn tolerance: the body
        // stops short by exactly as far as the animation aims off its own facing, so
        // the two compose onto the target.
        self.aim_offset = input.aim_offset;
        if let Some(bearing) = input.bearing_to_target {
            let goal = bearing + aim_error - self.aim_offset;
            let (yaw, rate) = zeroing::turn_toward(self.yaw, goal, dt);
            self.yaw = yaw;
            self.turn_rate = rate;
        } else {
            self.turn_rate = 0.0;
        }

        // ── Fire decision ──
        // Reaction served, in sight, and the true bearing within the firing cone
        // of where the barrel actually points. The bot is not asked whether it
        // will hit. The cone is measured off the BARREL, not the body: a hunter
        // mid-sideways-animation has its torso turned away on purpose, and gating on
        // the torso would stop it firing exactly when the animation says it should.
        let want_fire = match (input.bearing_to_target, self.target) {
            (Some(bearing), Some(_)) if in_sight && !self.bot_type.pacifist() => {
                self.zero.may_shoot(&tuning)
                    && angle_delta(self.barrel_yaw(), bearing).abs() <= FIRE_FOV_HALF_ANGLE
            }
            _ => false,
        };

        SimOutput {
            target_index: selection.index,
            yaw: self.yaw,
            barrel_yaw: self.barrel_yaw(),
            aim_error,
            zero_progress: self.zero.progress(&tuning),
            want_fire,
            speed_mult: self.speed_mult(),
        }
    }

    /// Record that `killer` killed this simulant, so a VengeSim can hunt them.
    pub fn note_killed_by(&mut self, killer: u32) {
        self.grudge.last_killer = Some(killer);
    }

    /// Respawn reset (`botmgr.c:229`).
    pub fn respawn(&mut self) {
        self.zero.reset();
        self.target = None;
        self.turn_rate = 0.0;
    }
}

/// Shortest signed angle from `from` to `to`, in `(-π, π]`.
pub fn angle_delta(from: f32, to: f32) -> f32 {
    let mut d = (to - from).rem_euclid(std::f32::consts::TAU);
    if d > std::f32::consts::PI {
        d -= std::f32::consts::TAU;
    }
    d
}
