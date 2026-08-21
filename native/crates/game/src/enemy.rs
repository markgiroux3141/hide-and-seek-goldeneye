//! A single hunter grunt. Extends the A1 perception FSM (ported from
//! `3DS FPS/src/ai/EnemyAI.ts`, `idle → alert → chase → attack ↔ cooldown`) with a
//! **search layer** so a hunter that floods in through the spawn door and does *not*
//! yet know where the player is will hunt for them rather than stand idle:
//!
//! * **Search** — no known target: walk to an assigned search point (the `World`
//!   hands out spread-out points so the pack fans out and sweeps the base), running
//!   the perception cone the whole time. Seeing the player promotes to `Alert`.
//! * **Alert → Chase → Attack ↔ Cooldown** — the original engagement chain, but
//!   the chase now paths to the player's **last-known position** (updated every step
//!   the player is perceived), so breaking line-of-sight makes the hunter go to where
//!   it last saw you rather than tracking you omnisciently.
//! * **Investigate** — lost the player (LOS broke / a heard gunshot): go to the
//!   last-known / noise position, scan around for a moment, then fall back to Search.
//! * **Fetch** — cannot shoot (holding nothing, or holding a gun with no rounds
//!   anywhere): walk to the pickup the `World` points at ([`Enemy::set_fetch_target`])
//!   and ignore the player entirely until armed. Outscores every other behaviour,
//!   because a hunter with nothing to shoot has no better option.
//!
//! Movement/perception constants are ported from `EnemyAI.ts`; the probabilistic
//! shot roll + the fire-animation cadence live in the `World` combat layer (which
//! owns the animation mixer + the player), driven by [`EnemyStep::want_fire`]. Search
//! coordination (which point each hunter gets) lives in `World` too — this file just
//! walks to whatever [`Self::assign_search_target`] set and reports when it needs a
//! fresh one via [`EnemyStep::needs_search_target`].
//!
//! Scope note (2026-07-16): door **breach/blocking is disabled** — doors are open
//! passages during the hunt — so the FSM has no door-blocking branch.
//!
//! Scope note (2026-08-17): a hunter can be flagged [`Enemy::set_omniscient`], which
//! swaps the *knowledge* rule (not the perception one) for Perfect Dark's — it always
//! knows where the player is and walks to the live position instead of a last-known
//! spot. See [`Enemy::known_target_pos`].

use glam::Vec3;
use rapier3d::prelude::ColliderHandle;

use engine::geometry::csg_runtime::WORLD_SCALE;
use engine::sim::nav::NavWorld;
use engine::sim::physics::PhysicsWorld;

use crate::pdsim::difficulty::BotDifficulty;
use crate::pdsim::distmode::{DistBand, DistMode, DistModeState};

/// **Which engagement model the hunters run** — `AI=pd|ours`, resolved from the
/// environment at boot exactly like `ARSENAL=` and `BODIES=`, and defaulting to ours.
///
/// Both AIs stay runnable; this picks between them, it does not delete either. What
/// each one owns is audited in `DESIGN_AI_PD_VS_OURS.md`, and the short version is
/// that they differ only in *what happens between the trigger and the feet*: the
/// Perfect Dark aim/fire model (`pdsim`) drives the gun in **both** modes, and so do
/// nav/A*, ORCA, wall clearance, foot IK, head look-at and animation.
///
/// ```text
/// AI=ours   (default) our handcrafted hunter: perception cone, search + investigate,
///                     a utility-scored FSM, standoff-and-hold combat, aim-dodge,
///                     flanking, cover/peek, burst-and-reposition, suppressing fire.
/// AI=pd               Perfect Dark's deathmatch simulant: omniscient, never searches,
///                     four-mode distance-band combat (`botcmd_tick_dist_mode`) with
///                     none of the evasive or tactical movement above, and PD's
///                     reload rule.
/// ```
///
/// `AI=pd` is a **faithful deathmatch AI in a hide-and-seek game**, which is a
/// deliberate choice and not an oversight: it always knows where you are and walks the
/// shortest path to your live position. That is what Perfect Dark's bots do, and
/// seeing how it feels against ours is the entire reason the switch exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiMode {
    /// Our handcrafted hunter (the default, and the shipping behaviour).
    Ours,
    /// Perfect Dark's simulant engagement model.
    Pd,
}

impl AiMode {
    /// Resolve from the `AI` environment variable, defaulting to [`AiMode::Ours`].
    ///
    /// Applied **last** at boot and logged, which is trap #6 from the weapons handoff:
    /// an explicit choice has to outrank a mode default, because `enable_pd_lab` once
    /// pinned a body set and silently ate `BODIES=ge` for an entire playtest.
    pub fn from_env() -> Self {
        let raw = std::env::var("AI").unwrap_or_default();
        match raw.trim().to_ascii_lowercase().as_str() {
            "pd" | "perfectdark" | "perfect-dark" | "sim" => AiMode::Pd,
            "" | "ours" | "our" | "hs" => AiMode::Ours,
            other => {
                log::warn!("AI={other:?} is not pd|ours — falling back to ours");
                AiMode::Ours
            }
        }
    }

    /// One line naming the resolved model, for the startup log. A resolved choice
    /// nobody can see is a choice that will be argued about later.
    pub fn summary(self) -> &'static str {
        match self {
            AiMode::Ours => "AI: ours — perception + search + utility FSM, standoff combat",
            AiMode::Pd => {
                "AI: Perfect Dark — omniscient, no search, distance-band combat (botcmd.c:39)"
            }
        }
    }

    pub fn is_pd(self) -> bool {
        self == AiMode::Pd
    }
}

/// Difficulty-scaled AI knobs threaded into [`Enemy::update`] each step (built by
/// `World` from its difficulty dial). Level 0 reproduces the baseline constants;
/// higher difficulty shortens the reaction + burst cooldown and raises `dodge`, the
/// lateral-evasion intensity that makes a hunter juke while it fights so the player
/// can't just hold the crosshair on it.
#[derive(Clone, Copy)]
pub struct AiTuning {
    /// Reaction delay before chasing (baseline [`ALERT_DURATION`]).
    pub alert: f32,
    /// Seconds between fire bursts (baseline [`COOLDOWN_DURATION`]).
    pub cooldown: f32,
    /// Reactive aim-dodge intensity, 0..1: 0 disables it (the hunter never jukes off
    /// your aim); higher makes it juke sooner/more often when it senses your crosshair.
    /// Gates + paces the reactive evade only — there is no passive constant weave.
    pub dodge: f32,
    /// Multiplier on engagement movement speed (chase / attack-advance / reposition).
    /// 1.0 = baseline; higher closes + repositions faster.
    pub speed_mult: f32,
    /// Perception-range multiplier (sight + proximity), 1.0 = baseline. Higher lets a
    /// harder hunter notice + keep tracking the player from further out — the "sharper
    /// senses" difficulty lever. Applied to [`DETECTION_RANGE`]/[`PROXIMITY_RANGE`].
    pub sense: f32,
    /// Suppressing-fire aggression, 0..1: 0 = the hunter only fires once it has closed
    /// to its weapon standoff (baseline); higher lets it open fire *while still closing*
    /// (the `Chase` state), out to an extra band that widens with this value. 0 at
    /// difficulty 0, so the baseline "close then fire" is unchanged.
    pub suppress: f32,
    /// Flanking intensity, 0..1: 0 = chase the player dead-straight (baseline); higher
    /// bends the approach onto an offset bearing so the hunter comes in from the side
    /// (and packmates split left/right), scaling the swing up to [`FLANK_MAX_ANGLE`].
    pub flank: f32,
    /// Cover-usage intensity, 0..1: 0 = never breaks LOS (baseline — the open
    /// burst-and-reposition juke only); higher makes the hunter duck to a no-LOS cell
    /// and peek-fire between bursts. Gated so that at low values only a *hurt* hunter
    /// takes cover, and at high values an unhurt one does too. Also shortens the dwell
    /// in cover (peeks more often). No effect where no cover cell exists.
    pub cover: f32,
    /// **Which engagement model this step runs** (`AI=pd|ours`). Under
    /// [`AiMode::Pd`] the `World` also zeroes `dodge` / `flank` / `cover` /
    /// `suppress` above, because Perfect Dark's bots have no equivalent of any of
    /// them — that is a flag, not a deletion, and flipping back to `ours` restores
    /// the lot. See [`AiMode`].
    pub mode: AiMode,
    /// The Perfect Dark difficulty tier the dial currently selects. Only
    /// [`AiMode::Pd`] reads it: `botcmd_tick_dist_mode` lets `MEAT`/`EASY` bots be
    /// crowded much closer before they back off (`botcmd.c:103`).
    pub tier: BotDifficulty,
}

impl Default for AiTuning {
    fn default() -> Self {
        Self {
            alert: ALERT_DURATION,
            cooldown: COOLDOWN_DURATION,
            dodge: 0.0,
            speed_mult: 1.0,
            sense: 1.0,
            suppress: 0.0,
            flank: 0.0,
            cover: 0.0,
            mode: AiMode::Ours,
            tier: BotDifficulty::Normal,
        }
    }
}

/// The widest a fully-flanking hunter (`flank` == 1) swings its approach bearing off
/// the direct line to the player (radians, ~50°). Scaled by `flank`, so it's 0 at
/// difficulty 0 (a dead-straight chase, baseline). Kept under a right angle so the
/// hunter still makes net progress toward the player rather than orbiting it.
const FLANK_MAX_ANGLE: f32 = 0.9;
/// A flanking hunter aims this fraction of its current player-distance *in* along the
/// offset bearing each step, so it both curves to the side AND keeps closing (rather
/// than circling at a fixed radius). Recomputed every step → a smooth curved approach.
const FLANK_CLOSE_FRAC: f32 = 0.6;

/// How far (m) beyond its normal attack range a fully-aggressive hunter
/// (`suppress` == 1) will open fire while still advancing — the width of the
/// suppressing-fire band, scaled by `suppress` (0 at difficulty 0). Kept modest so
/// suppressing fire reads as "firing as it closes," not sniping from across the map.
const SUPPRESS_BAND: f32 = 6.0; // m

/// **Who a hunter is currently engaging.** Every movement, cover and perception
/// decision in [`Enemy::update`] measures against this, so the FSM can hunt a
/// packmate the way it hunts the player.
///
/// The FSM used to take the player's position directly, which made hunter-on-hunter
/// combat half-real: a Perfect Dark simulant that picked a packmate would *shoot* it
/// (the round resolves against whoever is on the line — see `World::emit_pd_shot`) but
/// keep manoeuvring against the player, so it fired from wherever it happened to be
/// standing instead of closing, holding a standoff and taking cover. Threading the
/// target through instead is what turns that into an actual free-for-all.
///
/// A few rules stay **player-specific on purpose** and are not routed through here:
/// the catch check ([`Enemy::catches`]), the crosshair aim-sense that drives the
/// reactive dodge (there is no packmate crosshair), the `detectable` invisibility
/// toggle (see [`Enemy::can_see`]), and the `World`-level squad alert, footstep noise
/// and grenade flush.
#[derive(Clone, Copy, Debug)]
pub struct EngageTarget {
    /// The target's feet in world space.
    pub pos: Vec3,
    /// Which character that is.
    pub id: TargetId,
}

/// Which character an [`EngageTarget`] refers to. Hunters are identified by their
/// index in the `World`'s roster, matching `world::pd_lab::PdTarget`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TargetId {
    Player,
    Hunter(usize),
}

impl EngageTarget {
    /// The player, at `pos` — the default for every hunter that has not picked
    /// something else, and what the whole game was before free-for-all.
    pub fn player(pos: Vec3) -> Self {
        EngageTarget { pos, id: TargetId::Player }
    }

    /// Hunter `i`, at `pos`.
    pub fn hunter(i: usize, pos: Vec3) -> Self {
        EngageTarget { pos, id: TargetId::Hunter(i) }
    }

    /// Whether this is the player, which is what the deliberately player-only rules
    /// key off.
    pub fn is_player(&self) -> bool {
        matches!(self.id, TargetId::Player)
    }
}

/// The outcome of one enemy step, reported back to [`crate::world::World`].
#[derive(Default)]
pub struct EnemyStep {
    /// The player is within catch range this step (a melee fallback — largely
    /// dormant now that the hunter stops at attack range to shoot).
    pub caught: bool,
    /// The hunter wants to start a fire burst this step (it entered `attack` and
    /// isn't already firing). The `World` plays the fire one-shot on the shared
    /// animation mixer; the shot cadence + damage roll run there.
    pub want_fire: bool,
    /// The hunter is searching and has no (reachable) search point to head for —
    /// the `World` should hand it a fresh one via [`Enemy::assign_search_target`]
    /// (this is where the fan-out coordination lives, since one hunter can't see
    /// where the others are going).
    pub needs_search_target: bool,
}

/// The decision FSM: the A1 engagement chain (`EnemyAI.AIState`) plus the two
/// search-layer states that drive a hunter which doesn't yet know where the player
/// is (see the module docs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AiState {
    /// Standing still, unaware (the spawn-in state before a search point arrives,
    /// and the fallback if there's nowhere left to search).
    Idle,
    /// Sweeping the base toward an assigned search point, perception cone live.
    Search,
    /// Walking to a last-known / heard position, then scanning it, before giving up
    /// to Search.
    Investigate,
    Alert,
    Chase,
    Attack,
    Cooldown,
    /// Breaking contact to a nearby cell with NO line-of-sight to the player (cover),
    /// then holding there briefly — entered between bursts / when hurt at higher
    /// difficulty. Pairs with [`Self::Peek`] (#1 use-cover / break-LOS).
    TakeCover,
    /// Popping out of cover to a cell that DOES see the player, firing a burst, then
    /// ducking back to cover (#4 peek-and-fire).
    Peek,
    /// **Going shopping**: walking to the weapon or ammo pickup this hunter needs
    /// ([`Self::set_fetch_target`]), ignoring the player entirely on the way.
    ///
    /// A first-class behaviour rather than a reuse of the search channel, and that is
    /// the correction this state records. Fetching was originally smuggled through
    /// `assign_search_target` on the reasoning that a hunter which cannot shoot drops
    /// into the blind states and walks where it is told. It does not: `Search` scores
    /// zero the moment a `last_known` exists (one gunshot is enough), so the scorer
    /// picked `Investigate` — whose executor walks to the player's last-known position.
    /// An empty-handed hunter therefore beelined at the player holding nothing, and the
    /// test that was supposed to catch it asserted `!is_engaged()`, which `Investigate`
    /// satisfies. Fetching is a *behaviour*; it belongs in the behaviour scorer.
    Fetch,
}

const WT: f32 = WORLD_SCALE;
/// Per-state movement speeds (m/s) — chosen so the continuous locomotion blend
/// shows the full gait range: search/investigate walk, attack-advance jog, chase
/// run. `speed()` reports whichever the current step used so the legs match.
const SPEED_SEARCH: f32 = 1.6; // ~walk gait — calm sweeping / investigating
const SPEED_ADVANCE: f32 = 3.2; // ~jog gait — closing on the player while firing
/// Chase speed (JS `chaseSpeed`) — the urgent run.
pub(crate) const SPEED_CHASE: f32 = 4.6; // m/s (~run gait)
const REPATH_INTERVAL: f32 = 0.4; // s between path recomputes (CHASE_UPDATE_INTERVAL)

// ─── Anti-grind (crowd / separation-fight) ───────────────────────────────────
// When several hunters converge on the same spot, the per-step separation nudge
// (`world::separate_enemies`) fights their approach: each frame `move_toward` steps
// them in and separation shoves them back, so they jockey at full walk-speed while
// travelling ~nothing — the "manic strafing in place" jank. The `World` feeds each
// hunter its ACTUAL net displacement per step (`note_travel`); a hunter that intended
// to move but was held nearly in place for [`STUCK_TIME`] briefly holds
// ([`STUCK_HOLD`]) so it settles instead of grinding, and its reported gait speed
// tracks real travel so the legs idle rather than walk-cycle on the spot.
/// Intended speed (m/s) above which a step counts as "wanted to move."
const STUCK_INTENT: f32 = 0.3;
/// Window (s) over which NET progress is judged. A crowded hunter oscillates — it
/// moves at speed every frame but alternates direction — so we can't key off
/// instantaneous speed; we check how far it actually got over this window.
const STUCK_TIME: f32 = 0.5;
/// Minimum NET travel (m) over [`STUCK_TIME`] that counts as real progress; below
/// this while trying to move = grinding in the crowd → hold.
const STUCK_PROGRESS: f32 = 0.3;
/// Seconds a stuck hunter holds (legs idle) before trying to move again.
const STUCK_HOLD: f32 = 0.4;
const CATCH_DIST: f32 = 1.2 * WT; // 0.3 m — horizontal catch radius
const WAYPOINT_EPS: f32 = 0.4 * WT; // 0.1 m — advance to next waypoint within this
const CATCH_VERT: f32 = 3.0 * WT; // must be within ~1 floor vertically to catch
/// The straight-line beeline shortcut is only taken when the target is within this
/// height of the hunter (< half a 0.25 m stair step). Any real vertical traversal —
/// stairs, platforms, switchback landings — must go through A*, which walks cardinal
/// cell-to-cell and so can't cut a diagonal corner across an open stairwell (the
/// switchback "turns 180°, cuts through the railing gap, and falls" bug).
const COPLANAR_BEELINE_EPS: f32 = 0.5 * WT; // 0.125 m

/// Perception + FSM constants (JS `AIConfig` defaults + `EnemyManager` overrides).
const DETECTION_RANGE: f32 = 12.0; // m
const DETECTION_HALF_CONE: f32 = 60.0 * std::f32::consts::PI / 180.0; // 120° cone → ±60°
/// While actively **searching** (the blind states: Search / Investigate / Idle) a
/// hunter SWEEPS its perception — its view axis rotates a full circle (see
/// [`SEARCH_SWEEP_RATE`]) as if scanning the area — with this peripheral half-angle.
/// So a plainly-visible, in-range player is spotted within one sweep regardless of
/// which way the hunter is walking, instead of being ignored because it faces off.
/// Still LOS- and range-gated (a wall hides you). This is the fix for the "stands off
/// looking at me but won't engage" stall: `Search` used to look only along its travel
/// direction and never scanned, so a player in plain sight behind it went unnoticed.
const SEARCH_HALF_CONE: f32 = 70.0 * std::f32::consts::PI / 180.0; // ±70° peripheral
/// How fast the searching view axis sweeps (rad/s). A full circle every ~2.4 s — under
/// the 3 s stall threshold — so a visible player is always caught within one sweep.
const SEARCH_SWEEP_RATE: f32 = 2.6;
/// Fraction of the computed wall-clearance nudge applied per step — soft, so it eases
/// the body off walls over a few frames instead of a hard per-step shove that would
/// overpower ORCA's queueing in a tight pinch (a full shove broke the doorway funnel).
/// The steady-state clearance still reaches the full radius (penetration is re-probed
/// each step, so it eases in exponentially).
const WALL_CLEARANCE_STRENGTH: f32 = 0.4;

/// Peak head-scan swing (rad, ~46°) a blindly-hunting hunter turns its head left↔right
/// while its body faces its travel direction — the readable VISUAL analogue of the
/// invisible 360° perception sweep above. Kept inside the head look-at cone
/// (`ENEMY_HEAD_LOOK_CONE`) so the gaze never pins at the clamp. See
/// [`Enemy::head_scan_dir`].
const HEAD_SCAN_AMP: f32 = 0.8;
/// A player closer than this is noticed **regardless of facing** (you can't sneak
/// past a guard you're standing next to — footsteps/presence). Still LOS-gated, so a
/// wall between hides you. Kills the "walks off ignoring me while I'm right here" bug.
const PROXIMITY_RANGE: f32 = 3.5; // m
/// The advance-and-fire band inside the standoff: the hunter enters `Attack` (and
/// re-engages from `Cooldown`) once within `standoff + this`, then closes to the
/// per-weapon standoff and holds — so it fires *while moving* (run-and-gun) instead
/// of freezing at first sight of the player. Firing is a timer now, so movement +
/// shooting mix. The effective attack range is capped at [`DETECTION_RANGE`] (it
/// can't fight what it can't see). Replaces the old fixed `ATTACK_RANGE`; the
/// standoff it's measured from is now the weapon's ([`crate::combat::standoff_for`]).
const ATTACK_FIRE_BAND: f32 = 3.0; // m
/// Hysteresis around the standoff: once holding, the hunter only resumes closing
/// if the player pulls beyond `standoff + this`. Prevents the micro-step-in-and-out
/// at the exact boundary that kept the legs walking in place.
const STANDOFF_HYST: f32 = 1.2; // m
/// How long (s) line-of-sight to the player must stay broken before an `Attack`
/// hunter falls back to `Chase`. A brief flicker — the player flashing past a pillar
/// edge, or the hunter holding on a wall-corner sightline seam — shouldn't drop
/// engagement and bounce Attack↔Chase (a thrash the AI lab flagged once ORCA could
/// settle a hunter on such a seam). Genuine loss (beyond the grace) still bails.
const ATTACK_LOS_GRACE: f32 = 0.3; // s
pub(crate) const ALERT_DURATION: f32 = 0.5; // s reaction delay (level-0 baseline)
pub(crate) const COOLDOWN_DURATION: f32 = 1.5; // s between fire bursts (level-0 baseline)

// ─── Burst-and-reposition (Perfect Dark "sim" repositioning) ─────────────────
/// Between fire bursts, instead of standing at the standoff waiting out the
/// cooldown (a turret), the hunter jukes to a fresh firing position: it swings
/// this far around the player and re-engages from the new angle. No strafe clip is
/// needed — it faces its *travel* direction on the move (clean legs, like Chase),
/// then snaps back to face + fire when it plants. The direction flips each burst so
/// it weaves back and forth rather than orbiting one way.
const REPOSITION_ARC: f32 = 0.7; // rad (~40°) swung around the player per juke
/// Keep the reposition juke around the hunter's standoff (as a fraction of it), so a
/// sniper weaves at long range and a shotgunner weaves up close — rather than every
/// hunter re-closing to the same fixed band. Far enough to have moved, close enough
/// that it re-engages the instant it arrives.
const REPOSITION_MIN_FRAC: f32 = 0.8;
const REPOSITION_MAX_FRAC: f32 = 1.4;
/// Jog to the reposition spot (reads as a deliberate tactical relocate, not a panic
/// sprint or a stroll).
const REPOSITION_SPEED: f32 = SPEED_ADVANCE;

// ─── Use cover / break LOS + peek-and-fire (#1 / #4) ─────────────────────────
/// A hunter counts as "hurt" (and will always break to cover when `cover` > 0) once
/// its health drops below this fraction of its spawn max.
const COVER_HURT_FRAC: f32 = 0.6;
/// `cover` intensity at/above which an UNHURT hunter also breaks to cover between
/// bursts (below it, only a hurt hunter does). So the ramp is: low difficulty → never;
/// mid → cover when hurt; high → cover every reposition.
const COVER_UNHURT_MIN: f32 = 0.5;
/// Seconds a hunter dwells hidden in cover before peeking out, lerped by `cover`
/// (higher difficulty → shorter dwell → pops out more often).
const COVER_DWELL_LO: f32 = 0.9;
const COVER_DWELL_HI: f32 = 0.4;
/// Cover/peek cell search: this many bearings around the anchor, at each of these
/// radii (m). Bounded so the LOS-sampling cost is fixed (and it's only run at a
/// burst-end / peek transition, not every frame).
const COVER_SAMPLE_DIRS: usize = 12;
const COVER_SAMPLE_RADII: &[f32] = &[2.5, 4.5, 6.5];

// ─── Reactive aim-sense evasion ──────────────────────────────────────────────
/// When an engaged hunter senses the player's crosshair on it (`aimed_at`), it
/// commits a fast lateral juke for [`EVADE_BURST`] seconds to slide off the shot
/// line — sharper + more committed than the passive weave, so a lined-up shot tends
/// to whiff. Only fires when difficulty `dodge > 0`; the re-juke interval shrinks with
/// dodge (rare twitch at low difficulty → near-constant jinking at full).
const EVADE_SPEED: f32 = 6.5; // m/s lateral burst
const EVADE_BURST: f32 = 0.35; // s a committed juke lasts
/// Re-juke interval bounds (s), lerped by `dodge`: slower at low difficulty, snappier
/// at full. Floored well above the frame time so even at max difficulty each juke is a
/// discrete, readable reaction to the player's aim rather than a continuous blur.
const EVADE_INTERVAL_LO: f32 = 0.9;
const EVADE_INTERVAL_HI: f32 = 0.45;

// ─── Search layer ────────────────────────────────────────────────────────────
/// Within this XZ distance (m) of a search / investigate target, the hunter counts
/// as "arrived" (and Search asks for the next point).
const ARRIVE_DIST: f32 = 0.6;

/// Extra standoff beyond a door's own swing clearance, in metres — roughly a hunter's
/// capsule radius, so the panel misses its shoulders rather than grazing them.
///
/// The hold distance is `nav.door_clearance(i) + this` rather than a flat constant,
/// because a door that swings **toward** the hunter sweeps a quarter circle out from
/// its hinge as wide as the opening itself. Hunters navigate the grid and ignore the
/// panel's collider entirely, so nothing else stops the door passing through them —
/// they have to stand back far enough of their own accord.
const DOOR_WAIT_MARGIN: f32 = 0.45;
/// Longest a hunter will wait on a door before walking on regardless — a safety valve,
/// not a behaviour. A door it may open swings in well under a second, and one it may
/// not is already solid to its pathing, so this should never fire in practice; it
/// exists so no bug can leave a hunter frozen at a doorway for a whole round.
const DOOR_WAIT_MAX: f32 = 2.5;
/// How long (s) a hunter scans a spot in `Investigate` before giving up to `Search`.
const INVESTIGATE_SCAN_DURATION: f32 = 2.5;
/// How fast (rad/s) the hunter's facing sweeps while scanning in `Investigate`, so
/// its perception cone actually pans across the room to re-acquire the player.
const SCAN_TURN_RATE: f32 = 1.6;

/// Starting health (JS `EnemyCharacter` default + facility karl/joe). With PP7
/// damage 25 → 4 shots to kill.
pub const ENEMY_HEALTH: f32 = 100.0;

/// Utility-AI belief memory (s) — how long after last seeing the player a hunter stays
/// "engaged" (pursuing the last-known) before the belief decays to investigate/search.
/// The modern equivalent of the FSM's structural persistence (roadmap #4).
const ENGAGE_MEMORY: f32 = 5.0;
/// Anti-thrash inertia bonus added to the CURRENT behaviour's utility score, so a
/// near-tie at a band boundary sticks instead of flip-flopping (decision hysteresis —
/// the utility-layer analogue of the FSM's standoff/LOS-grace debouncing).
const UTIL_INERTIA: f32 = 0.2;

// ─── Going shopping ([`AiState::Fetch`]) ─────────────────────────────────────
/// The utility score of fetching. Deliberately **dominant** — clear of every other
/// behaviour plus [`UTIL_INERTIA`] — because a hunter that cannot shoot genuinely has no
/// better option, which makes this one of the few honestly unconditional scores in the
/// table. It is zero whenever the hunter has a working weapon, so an armed hunter never
/// sees it (`an_armed_hunter_ignores_the_pickups_and_fights`).
const FETCH_SCORE: f32 = 3.0;
/// Speed (m/s) a hunter travels to a pickup: a jog, not the full chase run. It wants the
/// gun urgently but it is not chasing anything.
const SPEED_FETCH: f32 = SPEED_ADVANCE;
/// How long (s) a fetching hunter may stand at its fetch point without the collect
/// landing before it writes that pickup off.
///
/// Arrival is nav-grid arrival, and the grid steers the hunter's *centre* to a cell —
/// so a gun on a ledge, or in a pocket A\* cannot path into, leaves a hunter parked
/// [`ARRIVE_DIST`] from something it can never pick up. Standing there forever is the
/// statue failure mode the AI lab exists to catch, so it gives up instead.
const FETCH_STALL: f32 = 3.0;
/// How long (s) fetching stays suppressed after a give-up, during which the hunter falls
/// back to its ordinary fan-out search. It re-asks afterwards, because the pickup may
/// simply have been contested — the World re-picks the nearest useful one every step.
const FETCH_RETRY: f32 = 5.0;

pub struct Enemy {
    /// Feet position, meters.
    pub pos: Vec3,
    /// Current path (meters waypoints), and the index we're heading toward.
    path: Vec<Vec3>,
    path_idx: usize,
    repath_timer: f32,
    /// The nav-overlay index of a shut door standing in this hunter's way, if any —
    /// drained into [`EnemyStep::open_door`] so the `World` (which owns the door
    /// entities and the audio) can work it. Cleared each step it isn't needed.
    pending_door: Option<usize>,
    /// How long this hunter has been held up at a shut door. A safety valve only: a
    /// door it may open swings in well under a second, so this fires solely if
    /// something has gone wrong, and it lets the hunter carry on rather than stand
    /// there forever.
    door_wait: f32,
    /// Horizontal facing (unit vector): the direction the model faces + the
    /// perception cone axis. Set to the travel direction while chasing and toward
    /// the player while alert/attack/cooldown (JS `faceTarget`).
    heading: Vec3,
    /// Whether the hunter advanced this step (false while idle/attacking/pathless).
    moving: bool,
    /// Speed (m/s) of the step actually taken this update — 0 when stationary. Set
    /// by [`Self::move_toward`] from the per-state speed; drives the locomotion gait.
    move_speed: f32,
    /// The **preferred velocity** the FSM wants this step (planar, XZ; Y is 0), before
    /// local avoidance. The movement helpers accumulate into it via [`Self::add_move`]
    /// instead of committing `pos` directly; the `World` runs ORCA over every hunter's
    /// preferred velocity + the player, then commits the resolved velocity through
    /// [`Self::integrate_move`]. Reset to zero at the top of each [`Self::update`].
    desired_vel: Vec3,
    /// The actual planar velocity committed last step (post-avoidance). Fed back into
    /// ORCA as this agent's current velocity so the reciprocal responsibility split is
    /// stable + smooth across frames. Zero until the first move.
    vel: Vec3,
    /// Remaining health; at ≤0 the hunter is [`Self::dead`] (Track A).
    health: f32,
    /// Killed — [`Self::update`] is a full no-op (the body holds its death pose
    /// while it fades). Set by [`Self::take_damage`] on the lethal shot.
    dead: bool,
    /// Hit-reaction "spaz-out" timer (s); while >0 the hunter stops moving so the
    /// hit one-shot reads (JS clears `moveTarget` during a hit).
    stun_timer: f32,

    // ─── A1 perception FSM ──
    state: AiState,
    alert_timer: f32,
    chase_timer: f32,
    cooldown_timer: f32,
    /// A fire burst has been requested this attack entry (JS `isAttacking`).
    is_attacking: bool,
    /// In `Attack`, whether the hunter has reached its standoff and is holding
    /// position (feet planted). Hysteresis flag — see [`STANDOFF_HYST`].
    holding: bool,
    /// Time (s) line-of-sight to the player has been continuously broken while in
    /// `Attack`. Debounces the Attack→Chase bail so a momentary flicker doesn't thrash
    /// — see [`ATTACK_LOS_GRACE`]. Reset whenever LOS is clear.
    attack_los_lost: f32,
    /// The fire animation has actually started playing (JS `fireAnimStarted`) —
    /// so we detect its *completion* (not just its not-yet-started frames).
    fire_started: bool,

    // ─── Search layer ──
    /// The point this hunter is sweeping toward while in `Search`. Assigned by the
    /// `World` (fan-out coordination); cleared on arrival, when the hunter reports
    /// [`EnemyStep::needs_search_target`].
    search_target: Option<Vec3>,
    /// Where the player was last perceived (or a heard gunshot) — the chase paths
    /// here, and `Investigate` walks here then scans it.
    last_known: Option<Vec3>,
    /// Seconds spent scanning the current spot in `Investigate`.
    scan_timer: f32,
    /// Anti-grind: time accumulated in the current progress window, the position the
    /// window was anchored at, and the remaining hold once it's judged stuck (see the
    /// anti-grind consts + [`Self::note_travel`]).
    stuck_accum: f32,
    stuck_anchor: Vec3,
    stuck_hold: f32,
    /// Sweeping scan angle (rad) while blindly hunting — the perception view axis
    /// rotates around the facing by this, so a searcher scans a full circle over time
    /// and can spot a player in plain sight behind it (see [`Self::perception_view`]).
    search_look: f32,

    // ─── Going shopping ([`AiState::Fetch`]) ──
    /// Where the pickup this hunter needs is, or `None` when it needs nothing (or
    /// nothing on the floor would help). Written by the `World` every step — it owns
    /// the pickups — via [`Self::set_fetch_target`].
    ///
    /// **Its own channel, deliberately not `search_target`.** The search point's owner
    /// is the `World`'s fan-out coordinator, which hands out a fresh one on arrival;
    /// two writers on one field is what made the search sweep and the shopping trip
    /// fight each other.
    fetch_target: Option<Vec3>,
    /// Seconds stood at a fetch point without the collect landing (see [`FETCH_STALL`]).
    fetch_stall: f32,
    /// Seconds fetching stays suppressed after giving up on an unreachable pickup
    /// (see [`FETCH_RETRY`]). Decays every step, in every decision layer.
    fetch_block: f32,

    // ─── Burst-and-reposition ──
    /// Which way the hunter arcs when it repositions between bursts (+1 / −1),
    /// flipped each juke so it weaves back and forth rather than orbiting the player.
    strafe_dir: f32,
    /// The spot the hunter is juking to during `Cooldown` (burst-and-reposition), or
    /// `None` to hold + face like a plain cooldown (no standable/LOS spot to juke to).
    reposition_target: Option<Vec3>,

    /// Which way the hunter last juked (+1 / −1); the reactive evade alternates off it
    /// so successive jukes weave to opposite sides.
    dodge_dir: f32,
    /// Which side this hunter flanks the player from (+1 / −1); assigned by the `World`
    /// at spawn (packmates split left/right) so the pack surrounds rather than
    /// conga-lines down one lane. Only takes effect when `AiTuning.flank > 0`.
    flank_dir: f32,

    // ─── Use cover / break LOS + peek (#1 / #4) ──
    /// Spawn (max) health — the denominator for the "hurt" check that gates breaking
    /// to cover. Tracked here since there's no separate max pool on the struct.
    max_health: f32,
    /// While in `TakeCover`, the hidden (no-LOS) cell we're relocating to; while in
    /// `Peek`, unused (the pop-out spot is `peek_target`). Cleared between cycles.
    cover_target: Option<Vec3>,
    /// While in `Peek`, the pop-out cell (has LOS to the player) we step out to.
    peek_target: Option<Vec3>,
    /// Seconds held hidden in `TakeCover` before peeking out.
    cover_timer: f32,

    // ─── Reactive aim-sense evasion ──
    /// Seconds remaining in a committed reactive juke (0 = not evading).
    evade_burst: f32,
    /// Which way the current reactive juke goes (+1 / −1).
    evade_dir: f32,
    /// Seconds until another reactive juke may trigger (rate-limits the dodging).
    evade_cooldown: f32,

    /// Whether the player is currently perceivable by this hunter. The `World` sets it
    /// each step from its player-visibility toggle (a dev/observe aid, bound to `N`);
    /// when false, ALL perception in [`Self::update`] fails (no sight, no proximity), so
    /// the hunter can never see / keep the player and drops back to searching — letting
    /// you walk around and watch the search + head-scan behaviour. Defaults to `true`.
    detectable: bool,
    /// Body half-width (m) for the movement-time wall-clearance nudge, or `0.0` to
    /// disable it. Set by the `World` each step from its `wall_clearance` flag; grid nav
    /// keeps only the hunter's CENTRE on walkable ground, so this keeps the wider model
    /// from clipping into walls (see [`engine::sim::nav::NavWorld::wall_clearance_offset`]).
    /// Defaults to `0.0`, so headless callers that don't set it are unaffected.
    wall_clearance_radius: f32,

    // ─── Utility-AI decision layer (roadmap #4) ──
    /// When set, [`Self::update`] picks the behaviour each tick with a scored selector
    /// ([`Self::util_step`]) instead of the hand-coded FSM transitions. The `World` sets
    /// it each step from its `utility_ai` flag; the FSM is the kill-switch (`false`).
    /// Defaults `false` so headless `Enemy` unit tests keep the FSM unless they opt in.
    utility: bool,
    /// Seconds since the player was last perceived (0 = seeing now). Drives the utility
    /// belief memory ([`ENGAGE_MEMORY`]). Large until the first sighting.
    since_seen: f32,
    /// Whether the post-acquisition reaction delay ([`AiTuning::alert`]) has been served
    /// for the current engagement (so `Alert` only scores high until it elapses). Reset
    /// when the engagement belief lapses.
    alert_served: bool,
    /// Utility edge flag: a fire burst just ended this tick, so next tick's scorer
    /// initiates the between-bursts move (break to cover if desired, else reposition).
    post_burst: bool,

    // ─── Perfect Dark knowledge rule ──
    /// **Omniscience** — this hunter always knows where the player is, so it navigates
    /// to the *live* position rather than a last-known one and never searches. The
    /// `World` sets it each step (PD-lab hunters only, see `World::pd_omniscience`).
    /// Perception is deliberately untouched: it still only *sees* what the raycast +
    /// cone allow. See [`Self::known_target_pos`]. Defaults `false`.
    omniscient: bool,

    // ─── Perfect Dark combat movement (`AI=pd` only) ──
    /// `botcmd_tick_dist_mode`'s per-bot state — the current distance mode, the
    /// anti-oscillation override and the 1 s command TTL. Advanced only by
    /// [`Self::pd_step`]; inert (and unread) under `AI=ours`.
    dist: DistModeState,
    /// A tiny xorshift, seeded per hunter at spawn. Its only consumer is the random
    /// 0.33–2.33 s `OK` hold PD arms when a backup loses sight (`botcmd.c:152`);
    /// keeping it on the hunter (rather than reaching for the `World`'s RNG) is what
    /// lets the headless AI lab replay a PD-mode run byte-for-byte.
    rng: u32,
}

impl Enemy {
    /// Spawn at `feet`, initially watching toward `watch` (into the room, so the
    /// perception cone faces where the player is likely to be), and starting in
    /// [`AiState::Search`] — a hunter that just flooded in through the door and is
    /// hunting for the player. The `World` hands it a search point on the first step.
    pub fn new(feet: Vec3, watch: Vec3) -> Self {
        let heading = {
            let flat = Vec3::new(watch.x - feet.x, 0.0, watch.z - feet.z);
            if flat.length_squared() > 1e-6 {
                flat.normalize()
            } else {
                Vec3::NEG_Z
            }
        };
        Enemy {
            pos: feet,
            path: Vec::new(),
            path_idx: 0,
            repath_timer: 0.0,
            pending_door: None,
            door_wait: 0.0,
            heading,
            moving: false,
            move_speed: 0.0,
            desired_vel: Vec3::ZERO,
            vel: Vec3::ZERO,
            health: ENEMY_HEALTH,
            dead: false,
            stun_timer: 0.0,
            state: AiState::Search,
            alert_timer: 0.0,
            chase_timer: 0.0,
            cooldown_timer: 0.0,
            is_attacking: false,
            holding: false,
            attack_los_lost: 0.0,
            fire_started: false,
            search_target: None,
            last_known: None,
            scan_timer: 0.0,
            stuck_accum: 0.0,
            stuck_anchor: Vec3::ZERO,
            stuck_hold: 0.0,
            search_look: 0.0,
            fetch_target: None,
            fetch_stall: 0.0,
            fetch_block: 0.0,
            strafe_dir: 1.0,
            reposition_target: None,
            dodge_dir: 1.0,
            flank_dir: 1.0,
            max_health: ENEMY_HEALTH,
            cover_target: None,
            peek_target: None,
            cover_timer: 0.0,
            evade_burst: 0.0,
            evade_dir: 1.0,
            evade_cooldown: 0.0,
            detectable: true,
            wall_clearance_radius: 0.0,
            utility: false,
            since_seen: 1e6,
            alert_served: false,
            post_burst: false,
            omniscient: false,
            dist: DistModeState::default(),
            // Seeded from the spawn position so each hunter draws its own sequence and
            // a replayed run reproduces exactly (spawns are deterministic); never zero,
            // which is xorshift's fixed point.
            rng: (feet.x.to_bits() ^ feet.z.to_bits().rotate_left(16) ^ 0x9E37_79B9) | 1,
        }
    }

    /// One uniform draw in `[0,1)` from this hunter's own xorshift. See [`Self::rng`].
    fn rand01(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Set whether the player is perceivable by this hunter (the `World`'s
    /// player-visibility toggle). When `false`, perception is disabled and the hunter
    /// reverts to searching. See [`Self::detectable`].
    pub fn set_detectable(&mut self, v: bool) {
        self.detectable = v;
    }

    /// Enable/disable the utility-AI decision layer (roadmap #4). The `World` sets this
    /// each step from its `utility_ai` flag; `false` runs the legacy FSM (kill-switch).
    pub fn set_utility(&mut self, on: bool) {
        self.utility = on;
    }

    /// Enable/disable **omniscience** — Perfect Dark's knowledge rule. See
    /// [`Self::omniscient`] and [`Self::known_target_pos`]. The `World` sets it each
    /// step; `false` is our original perceive-then-remember behaviour (kill-switch).
    pub fn set_omniscient(&mut self, on: bool) {
        self.omniscient = on;
    }

    /// Whether this hunter always knows where the player is.
    pub fn is_omniscient(&self) -> bool {
        self.omniscient
    }

    /// **Where this hunter believes the player is** — the single knowledge source every
    /// movement decision reads, so omniscience is a *policy* rather than a rewrite.
    ///
    /// * Normally: [`Self::last_known`], written only when the player is actually
    ///   perceived (or a noise is heard). Break line-of-sight and it goes stale, which
    ///   is what makes a hunter walk to where you *were* and then fan out searching.
    /// * Omniscient: the player's live position, always. This is
    ///   `botcmd_tick_dist_mode`'s rule (`botcmd.c:39`) — a PD bot re-issues
    ///   `chr_go_to_prop(chr, targetprop, GOPOSFLAG_RUN)` against the target prop's
    ///   *current* coordinates whether or not it can see it, and
    ///   `bot_choose_general_target` (`bot.c:1589`) hands it the closest out-of-sight
    ///   chr when nobody is in sight. There is no last-known-position anywhere in the
    ///   simulant path.
    ///
    /// **Knowledge is not perception.** This never claims the hunter can *see* you:
    /// `perceived` / `perception_los` / `since_seen` are untouched, so the aim model
    /// (fed a real raycast on purpose — see `world::pd_lab`) and the LOS-gated `Attack`
    /// state keep the contract they were tuned against. An omniscient hunter with no
    /// sightline knows exactly where to walk and still cannot shoot through the wall.
    fn known_target_pos(&self, target_pos: Vec3) -> Option<Vec3> {
        if self.omniscient {
            Some(target_pos)
        } else {
            self.last_known
        }
    }

    /// Where a hunter goes when it loses contact mid-engagement.
    ///
    /// Normally `Investigate` — walk to the last-known spot, scan it, then give up to
    /// the fan-out `Search`. An **omniscient** hunter never loses contact (PD's bot
    /// keeps its target and simply re-paths to its live position), so it drops to
    /// `Chase` instead and keeps coming. `Search` / `Investigate` are unreachable for
    /// it, and so is `pick_search_point`.
    fn lose_contact(&mut self) {
        if self.omniscient {
            self.state = AiState::Chase;
            self.chase_timer = 0.0;
        } else {
            self.state = AiState::Investigate;
            self.scan_timer = 0.0;
        }
    }

    /// Set the body half-width (m) for the wall-clearance nudge (`0.0` disables it).
    /// See [`Self::wall_clearance_radius`].
    pub fn set_wall_clearance_radius(&mut self, r: f32) {
        self.wall_clearance_radius = r;
    }

    /// The point this hunter is currently sweeping toward in `Search` (so the
    /// `World` can fan the pack out — avoid handing two hunters the same point).
    pub fn search_target(&self) -> Option<Vec3> {
        self.search_target
    }

    /// Hand this hunter a fresh search point (the `World`'s fan-out coordinator).
    /// A no-op once dead. Keeps the hunter in / returns it to `Search`.
    pub fn assign_search_target(&mut self, target: Vec3) {
        if self.dead {
            return;
        }
        self.search_target = Some(target);
        if matches!(self.state, AiState::Idle) {
            self.state = AiState::Search;
        }
    }

    /// **Where this hunter should go shopping**, or `None` if it can fight (or nothing
    /// on the floor would help). Set by the `World` every step from
    /// `World::hunter_fetch_target`, which owns the pickups.
    ///
    /// While this is `Some`, [`AiState::Fetch`] outscores every other behaviour and the
    /// hunter walks here ignoring the player — see [`FETCH_SCORE`]. A no-op once dead.
    pub fn set_fetch_target(&mut self, target: Option<Vec3>) {
        if self.dead {
            return;
        }
        // A brand-new destination is a fresh trip: drop the give-up clock so a stall
        // against the *previous* pickup can't cut this one short.
        let moved = match (self.fetch_target, target) {
            (Some(a), Some(b)) => a.distance(b) > ARRIVE_DIST,
            (a, b) => a.is_some() != b.is_some(),
        };
        if moved {
            self.fetch_stall = 0.0;
        }
        self.fetch_target = target;
    }

    /// The pickup this hunter is walking to, if any — read by the head look-at (a
    /// shopping hunter looks at what it is going for) and by the AI lab.
    pub fn fetch_target(&self) -> Option<Vec3> {
        self.fetch_target
    }

    /// Whether this hunter is currently going shopping — the fetch behaviour is live
    /// AND not suppressed by a give-up. The single predicate all three decision layers
    /// gate on, so they cannot disagree about it.
    fn wants_fetch(&self) -> bool {
        self.fetch_target.is_some() && self.fetch_block <= 0.0
    }

    /// Set which side this hunter flanks from (+1 / −1) — the `World` assigns it at
    /// spawn so the pack splits left/right. Sign only; magnitude is ignored.
    pub fn set_flank_side(&mut self, dir: f32) {
        self.flank_dir = if dir < 0.0 { -1.0 } else { 1.0 };
    }

    /// The shut door standing in this hunter's way, if any (nav-overlay index) — the
    /// `World` works it, since doors are ECS entities with audio that the hunter can't
    /// reach from here.
    ///
    /// Read **straight off the hunter** rather than returned in [`EnemyStep`]. `update`
    /// has three separate return points (Perfect Dark's ladder, the utility scorer, the
    /// legacy FSM) and only one of them is live at a time, so a field on the step struct
    /// is only ever populated by whichever branch remembered to set it — which is
    /// exactly how the first version of this shipped broken, dropping every request
    /// because the default-on utility layer returns early.
    pub fn pending_door(&self) -> Option<usize> {
        self.pending_door
    }

    /// Report this hunter's ACTUAL net horizontal travel (`actual_m`) over the step
    /// just taken — called by the `World` after movement AND the separation nudge, so
    /// it captures the true displacement, not just the intended step. If the hunter
    /// wanted to move ([`STUCK_INTENT`]) but was held nearly in place ([`STUCK_SPEED`])
    /// for [`STUCK_TIME`] — the crowd / separation fight — it briefly holds so it
    /// settles instead of grinding a walk cycle on the spot. Also rewrites the reported
    /// gait speed to the real travel, so the legs idle when it isn't going anywhere.
    pub(crate) fn note_travel(&mut self, actual_m: f32, dt: f32) {
        if self.dead || dt <= 0.0 {
            return;
        }
        let actual = actual_m / dt;
        let intended = self.move_speed; // set by `move_toward` earlier this step
        // Judge NET progress over a window (oscillating-in-place has high instantaneous
        // speed but goes nowhere, so instantaneous speed can't tell us it's stuck).
        if intended > STUCK_INTENT {
            if self.stuck_accum == 0.0 {
                self.stuck_anchor = self.pos;
            }
            self.stuck_accum += dt;
            if self.stuck_accum >= STUCK_TIME {
                let net = Vec3::new(
                    self.pos.x - self.stuck_anchor.x,
                    0.0,
                    self.pos.z - self.stuck_anchor.z,
                )
                .length();
                if net < STUCK_PROGRESS {
                    self.stuck_hold = STUCK_HOLD; // tried but got nowhere → settle
                }
                self.stuck_accum = 0.0;
                self.stuck_anchor = self.pos;
            }
        } else {
            self.stuck_accum = 0.0; // not trying to move → not stuck
        }
        // Gait tracks real per-step travel (never above the intent) → legs idle when
        // settled/held instead of walk-cycling on the spot.
        self.move_speed = actual.min(intended);
    }

    /// React to a heard noise (e.g. the player's gunfire) at `pos`: if the hunter is
    /// still hunting blind (searching / investigating / idle), converge on the sound
    /// to investigate it. A hunter already engaged (alerted / chasing / attacking)
    /// keeps its better information. No-op once dead.
    pub fn hear_noise(&mut self, pos: Vec3) {
        if self.dead {
            return;
        }
        if matches!(self.state, AiState::Search | AiState::Investigate | AiState::Idle) {
            self.last_known = Some(pos);
            self.search_target = None;
            self.scan_timer = 0.0;
            self.state = AiState::Investigate;
        }
    }

    /// Horizontal facing (unit vector) — the direction the model faces.
    pub fn heading(&self) -> Vec3 {
        self.heading
    }

    /// A cosmetic head-scan DIRECTION for the blind states (Search / Idle): the body
    /// heading swung left↔right by a smooth ±[`HEAD_SCAN_AMP`] oscillation, reusing the
    /// perception sweep phase ([`Self::search_look`]) so it advances in step and paces
    /// with the scan. The perception cone does a full 360° sweep for gameplay
    /// ([`Self::perception_view`]) while the body faces its travel direction; this is
    /// the readable VISUAL analogue — a searching hunter turning its head side to side —
    /// for the head look-at to track. Stays inside the head cone so the gaze never pins.
    pub fn head_scan_dir(&self) -> Vec3 {
        let off = self.search_look.sin() * HEAD_SCAN_AMP;
        let (s, c) = off.sin_cos();
        let h = self.heading;
        Vec3::new(h.x * c - h.z * s, 0.0, h.x * s + h.z * c).normalize_or_zero()
    }

    /// Current speed (m/s): the speed of the step taken this update (per-state),
    /// or 0 when stationary. Drives the continuous locomotion gait.
    pub fn speed(&self) -> f32 {
        self.move_speed
    }

    /// The preferred (pre-avoidance) planar velocity the FSM wants this step — the
    /// `World`'s ORCA solve reads this as the agent's goal velocity.
    pub(crate) fn desired_velocity(&self) -> Vec3 {
        self.desired_vel
    }

    /// The actual velocity committed last step, fed back into ORCA for a stable
    /// reciprocal split.
    pub(crate) fn velocity(&self) -> Vec3 {
        self.vel
    }

    /// The speed (m/s) the FSM intended this step — the ORCA max-speed cap for this
    /// hunter, so avoidance can't make it move faster than its own state wanted.
    pub(crate) fn move_intent(&self) -> f32 {
        self.move_speed
    }

    /// Accumulate a preferred planar velocity for this step (additive, so a radial
    /// standoff move and a lateral evade combine — the same net displacement the old
    /// sequential `pos` steps produced). `dir` need not be unit; the Y component is
    /// dropped (hunters move on the nav floor — Y comes from [`Self::snap_to_floor`]).
    fn add_move(&mut self, dir: Vec3, speed: f32) {
        let flat = Vec3::new(dir.x, 0.0, dir.z);
        if flat.length_squared() < 1e-12 || speed <= 0.0 {
            return;
        }
        self.desired_vel += flat.normalize() * speed;
        self.move_speed = self.move_speed.max(speed);
        self.moving = true;
    }

    /// Commit an avoidance-resolved planar velocity `vel` for `dt`. Nav/LOS-gated +
    /// floor-snapped exactly like the old movement helpers, so avoidance never clips a
    /// wall or walks the hunter off a ledge: if the resolved step is blocked (ORCA
    /// deflected it into geometry) it falls back to the raw preferred velocity — which
    /// the FSM already vetted as nav-clear — and otherwise holds. Persists the actual
    /// committed velocity for next step's reciprocal ORCA. Called by the `World` after
    /// the FSM + ORCA solve (see `world::lifecycle`); a no-op once dead.
    pub(crate) fn integrate_move(&mut self, vel: Vec3, dt: f32, nav: &NavWorld) {
        if self.dead {
            self.vel = Vec3::ZERO;
            return;
        }
        let start = self.pos;
        let planar = Vec3::new(vel.x, 0.0, vel.z);
        if !self.try_step(planar, dt, nav) {
            let pref = self.desired_vel;
            self.try_step(pref, dt, nav);
        }
        // Wall-clearance: nudge the centre off any wall the (wider) body would clip.
        // Push-not-block, so it never stops the hunter fitting through a doorway.
        if self.wall_clearance_radius > 0.0 {
            let mut off = nav.wall_clearance_offset(self.pos, self.wall_clearance_radius);
            // Keep only the part of the nudge that doesn't OPPOSE intended travel, so it
            // declutters the body laterally off a wall but never brakes a hunter driving
            // forward into a doorway (a wall's front face would otherwise shove it back
            // out — the pack-funnel regression the lab caught).
            let dir = planar.normalize_or_zero();
            if dir != Vec3::ZERO {
                let along = off.dot(dir);
                if along < 0.0 {
                    off -= dir * along;
                }
            }
            // Apply SOFTLY (a fraction per step) so it eases the body off walls over a
            // few frames rather than hard-centring it — a full shove each step overpowers
            // ORCA's queueing in a tight pinch (broke the doorway funnel; the steady
            // state still converges to full clearance since the penetration is re-probed
            // every step).
            off *= WALL_CLEARANCE_STRENGTH;
            if off.length_squared() > 1e-8 {
                self.pos.x += off.x;
                self.pos.z += off.z;
                self.snap_to_floor(nav);
            }
        }
        let actual = Vec3::new(self.pos.x - start.x, 0.0, self.pos.z - start.z);
        self.vel = if dt > 1e-6 { actual / dt } else { Vec3::ZERO };
    }

    /// Try to move `v·dt` in the plane from the current position: applies + snaps to
    /// the floor if the step stays on clear, continuous ground, else leaves `pos`
    /// untouched. Returns whether it moved. The nav gate is the same LOS + ground
    /// continuity check the beeline / back-off / lateral steps use.
    fn try_step(&mut self, v: Vec3, dt: f32, nav: &NavWorld) -> bool {
        let flat = Vec3::new(v.x, 0.0, v.z);
        if flat.length_squared() < 1e-12 {
            return false;
        }
        let dest = self.pos + flat * dt;
        let up = Vec3::new(0.0, 0.5, 0.0);
        if nav.los_clear(self.pos + up, dest + up) && nav.ground_path_clear(self.pos, dest) {
            self.pos = Vec3::new(dest.x, self.pos.y, dest.z);
            self.snap_to_floor(nav);
            true
        } else {
            false
        }
    }

    /// The current FSM state (for inspection / tests).
    pub fn state(&self) -> AiState {
        self.state
    }

    /// The Perfect Dark distance mode this hunter is in — meaningful only after a
    /// step under `AI=pd` (see [`Self::pd_step`]). The AI lab reads it to check that
    /// a PD-mode hunter actually *stands still in band* rather than orbiting.
    #[allow(dead_code)] // the AI lab (`#[cfg(test)]`) is its only consumer
    pub(crate) fn dist_mode(&self) -> DistMode {
        self.dist.mode()
    }

    /// Seconds since this hunter last perceived its target (0 = seeing it now). PD's
    /// reload rule needs it: below half a clip, a bot only reloads once the target has
    /// been out of sight for 2 s (`bot.c:2470`).
    pub(crate) fn time_since_seen(&self) -> f32 {
        self.since_seen
    }

    /// Whether the hunter is actively engaged with the player (has eyes on it or is
    /// running the engagement chain) — the squad-alert broadcaster.
    pub fn is_engaged(&self) -> bool {
        matches!(
            self.state,
            AiState::Alert
                | AiState::Chase
                | AiState::Attack
                | AiState::Cooldown
                | AiState::TakeCover
                | AiState::Peek
        )
    }

    /// Where this hunter last perceived the player (or a heard noise) — the position
    /// an engaged hunter calls its packmates to converge on.
    pub fn last_known(&self) -> Option<Vec3> {
        self.last_known
    }

    /// Apply `dmg` to the hunter; returns `true` if this shot killed it (health
    /// crossed to ≤0). A dead hunter takes no further damage. Mirrors JS
    /// `Actor.takeDamage` (armor omitted — the grunt has none).
    pub fn take_damage(&mut self, dmg: f32) -> bool {
        if self.dead {
            return false;
        }
        self.health -= dmg;
        if self.health <= 0.0 {
            self.health = 0.0;
            self.dead = true;
            self.moving = false;
        }
        self.dead
    }

    /// Set the hunter's health (difficulty survivability scaling: at spawn, or a
    /// heal-to-new-max when the difficulty dial changes mid-hunt). No-op once dead —
    /// a corpse stays down. There's no separate max field; this is the current pool.
    pub fn set_max_health(&mut self, hp: f32) {
        if self.dead {
            return;
        }
        self.health = hp;
        self.max_health = hp;
    }

    /// Whether the hunter is wounded past the cover threshold — below
    /// [`COVER_HURT_FRAC`] of its spawn max. Drives the "break to cover when hurt"
    /// trigger (#1).
    fn is_hurt(&self) -> bool {
        self.max_health > 0.0 && self.health < self.max_health * COVER_HURT_FRAC
    }

    /// Whether the hunter has been killed.
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// Remaining health (for inspection / tests).
    pub fn health(&self) -> f32 {
        self.health
    }

    /// The health this hunter spawned with — the denominator for a health readout.
    /// Difficulty-scaled at spawn, so it is not [`ENEMY_HEALTH`] in general.
    pub fn max_health(&self) -> f32 {
        self.max_health
    }

    /// Stun the hunter for `dur` seconds — it stops moving while a hit reaction
    /// plays. Refreshes (does not stack) so a fresh hit restarts the window.
    pub fn stun(&mut self, dur: f32) {
        self.stun_timer = self.stun_timer.max(dur);
        self.moving = false;
    }

    /// Whether the hunter has clear line-of-sight to what it is engaging.
    ///
    /// The `detectable` invisibility toggle (`N`) applies **only to the player**: it is
    /// a dev/observe aid for watching hunters work without being shot, and extending it
    /// to packmates would blind the whole squad to each other whenever it was on.
    fn can_see(&self, target: EngageTarget, physics: &mut PhysicsWorld) -> bool {
        (!target.is_player() || self.detectable)
            && perception_los(physics, self.pos, target.pos)
    }

    /// Whether the **player** is within catch range (the melee fallback). Deliberately
    /// not routed through [`EngageTarget`]: a hunter cannot "catch" a packmate, and
    /// this is the check that ends the hunt.
    fn catches(&self, player_feet: Vec3) -> bool {
        self.dist_to(player_feet) < CATCH_DIST
            && (player_feet.y - self.pos.y).abs() < CATCH_VERT
    }

    /// Face the engage target instantly (JS `faceTarget`).
    fn face(&mut self, target_pos: Vec3) {
        let flat = Vec3::new(target_pos.x - self.pos.x, 0.0, target_pos.z - self.pos.z);
        if flat.length_squared() > 1e-6 {
            self.heading = flat.normalize();
        }
    }

    /// Horizontal (XZ) distance to a point — `prop_get_lateral_distance_to_prop`
    /// (`chraction.c:14030`). This is the arrival test: "have I got to that nav cell",
    /// where height is the floor's business and not the decision's.
    fn dist_to(&self, target_pos: Vec3) -> f32 {
        Vec3::new(target_pos.x - self.pos.x, 0.0, target_pos.z - self.pos.z).length()
    }

    /// **Full 3D distance** to a point — `prop_get_distance_to_prop`
    /// (`chraction.c:14021`), which is what `botcmd_tick_dist_mode` measures the attack
    /// band against (`botcmd.c:98`).
    ///
    /// Perfect Dark has both functions and uses this one for engagement, deliberately.
    /// We used the lateral one everywhere, and the consequence is visible the moment a
    /// level has floors: a hunter one storey below its target reads a comfortable 5 m,
    /// decides it is exactly at its standoff, plants — and has no shot, because the real
    /// separation is 5 m across and 3 m up, through a floor slab. It then holds there
    /// indefinitely instead of taking the stairs.
    ///
    /// Perception is left lateral on purpose: [`DETECTION_RANGE`] and the cone were
    /// tuned as a ground-plane reach and PD's own sight check is a raycast, not a
    /// distance. What moves to 3D here is the *engagement band* — enter, hold, back off,
    /// re-acquire — which is the part that was making decisions about a target it could
    /// not reach.
    fn engage_dist(&self, target_pos: Vec3) -> f32 {
        (target_pos - self.pos).length()
    }

    /// Whether the hunter perceives its engage target this step, given whether it has
    /// clear line-of-sight. Two LOS-gated ways in: the 120° detection cone out to
    /// [`DETECTION_RANGE`], or bare proximity within [`PROXIMITY_RANGE`] regardless of
    /// facing (can't sneak past a guard you're standing next to).
    fn perceives(&self, target_pos: Vec3, has_los: bool, sense: f32, look: Vec3, half_cone: f32) -> bool {
        if !has_los {
            return false;
        }
        let dist = self.dist_to(target_pos);
        dist < PROXIMITY_RANGE * sense
            || (dist < DETECTION_RANGE * sense && self.in_cone(target_pos, look, half_cone))
    }

    /// Whether the player is inside the perception cone of half-angle `half_cone` about
    /// the view axis `look` (JS `isTargetInCone`). `look` is the facing for an engaged
    /// hunter, or the sweeping scan axis while searching.
    fn in_cone(&self, target_pos: Vec3, look: Vec3, half_cone: f32) -> bool {
        let to = Vec3::new(target_pos.x - self.pos.x, 0.0, target_pos.z - self.pos.z);
        if to.length_squared() < 1e-6 {
            return true;
        }
        look.angle_between(to.normalize()) < half_cone
    }

    /// The perception view axis + cone half-angle for this step. Engaged hunters look
    /// along their facing (`heading`) with the narrow cone; blindly-hunting ones
    /// (Search / Investigate / Idle) SWEEP a wide peripheral cone a full circle around
    /// their facing (advancing [`Self::search_look`]) so they scan the area — this is
    /// what lets a searcher spot a player in plain sight behind it. Perception-only:
    /// the model keeps facing its travel direction.
    fn perception_view(&mut self, dt: f32) -> (Vec3, f32) {
        // `Fetch` counts as blindly hunting: it is walking somewhere with no target, and
        // a hunter crossing a room for a gun should still notice a player in plain sight
        // on the way (it is the `World` that suppresses its knowledge while shopping —
        // that is a policy about what it may act on, not about where it looks).
        if matches!(
            self.state,
            AiState::Search | AiState::Investigate | AiState::Idle | AiState::Fetch
        ) {
            self.search_look += SEARCH_SWEEP_RATE * dt;
            let (s, c) = self.search_look.sin_cos();
            let h = self.heading;
            let axis = Vec3::new(h.x * c - h.z * s, 0.0, h.x * s + h.z * c);
            (axis.normalize_or_zero(), SEARCH_HALF_CONE)
        } else {
            (self.heading, DETECTION_HALF_CONE)
        }
    }

    /// Advance the FSM one step against `target` — whoever this hunter is engaging (see
    /// [`EngageTarget`]). `player_feet` is passed separately and used **only** for the
    /// player-specific catch check, so a hunter chasing a packmate cannot end the hunt.
    ///
    /// `standoff` is the distance (m) this hunter holds at while attacking — the
    /// weapon's ([`crate::combat::EnemyWeaponDef::standoff`]), threaded in so a sniper
    /// hangs back and a shotgunner charges in. `band` is the same weapon's Perfect Dark
    /// engagement band (`g_BotDistConfigs`), read **instead** of `standoff` under
    /// `AI=pd`. `fire_anim` = a fire one-shot is
    /// currently playing on the shared mixer (the JS `enemyState === 'action'` proxy,
    /// disambiguated from hit/death by the caller). Returns `want_fire` when it wants
    /// the caller to start a fire burst this step, and `needs_search_target` when it's
    /// searching and needs the `World` to hand it a fresh point.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        dt: f32,
        target: EngageTarget,
        player_feet: Vec3,
        standoff: f32,
        band: DistBand,
        tuning: AiTuning,
        aimed_at: bool,
        nav: &NavWorld,
        physics: &mut PhysicsWorld,
        fire_anim: bool,
        self_collider: ColliderHandle,
    ) -> EnemyStep {
        // Difficulty-scaled perception reach: a harder hunter sees + tracks from
        // further (baseline `sense` == 1.0 → the original DETECTION_RANGE). Every
        // range gate in this step is measured off this, so the whole engagement
        // envelope grows together.
        let perception = DETECTION_RANGE * tuning.sense;
        // The hunter enters/holds the fight within `standoff + band`, capped at the
        // perception range (it can't engage what it can't see). Both the standoff and
        // this attack range now scale with the equipped weapon.
        let attack_range = (standoff + ATTACK_FIRE_BAND).min(perception);
        // Every geometric decision below is against the ENGAGE TARGET, which is the
        // player for an ordinary hunter and may be a packmate for a simulant.
        let target_pos = target.pos;
        self.moving = false;
        self.move_speed = 0.0;
        self.desired_vel = Vec3::ZERO;
        if self.dead {
            return EnemyStep::default();
        }
        // Stunned (mid hit-reaction): drain the timer, don't move or think.
        if self.stun_timer > 0.0 {
            self.stun_timer = (self.stun_timer - dt).max(0.0);
            return EnemyStep::default();
        }

        // Perception is checked every step, in every state: seeing the player is what
        // promotes a searcher to the engagement chain, and keeps the last-known
        // position fresh while chasing/attacking. Two ways in, both LOS-gated: the
        // 120° detection cone out to `DETECTION_RANGE`, OR close proximity regardless
        // of facing (`PROXIMITY_RANGE`).
        let has_los = self.can_see(target, physics);
        let (look, half_cone) = self.perception_view(dt);
        let perceived = self.perceives(target_pos, has_los, tuning.sense, look, half_cone);
        if perceived {
            self.last_known = Some(target_pos);
            self.since_seen = 0.0;
        } else {
            self.since_seen += dt;
        }
        // Utility belief lapse: once the player's been lost past the memory window, a
        // fresh acquisition re-serves the alert reaction delay (harmless to the FSM path).
        // An omniscient hunter's belief never lapses — it has not lost anything — so it
        // serves its reaction ONCE and then presses on. (Without this exemption it would
        // re-arm the delay every tick it couldn't see you and stand in `Alert` forever.)
        if self.since_seen >= ENGAGE_MEMORY && !self.omniscient {
            self.alert_served = false;
        }

        // Reactive aim-sense evasion: decay the timers, and if the player draws a bead
        // (`aimed_at`) on a hunter that's planted + firing — at its standoff (`Attack`)
        // OR popped out of cover (`Peek`) — kick off a committed lateral juke to slide
        // off the shot line. Rate-limited so it jinks rather than spasms; disabled at
        // difficulty 0 (dodge == 0). Only these two states (where it's otherwise a
        // near-stationary firing target); `Chase`/`Cooldown`/`TakeCover` are already
        // moving or hidden. The matching arm consumes the burst via `evade_step`.
        self.evade_cooldown = (self.evade_cooldown - dt).max(0.0);
        self.evade_burst = (self.evade_burst - dt).max(0.0);
        if aimed_at
            && tuning.dodge > 0.0
            && matches!(self.state, AiState::Attack | AiState::Peek)
            && self.evade_cooldown <= 0.0
            && self.evade_burst <= 0.0
        {
            self.evade_dir = -self.dodge_dir; // break off the current weave line
            self.dodge_dir = self.evade_dir;
            self.evade_burst = EVADE_BURST;
            self.evade_cooldown =
                EVADE_INTERVAL_LO + (EVADE_INTERVAL_HI - EVADE_INTERVAL_LO) * tuning.dodge;
        }

        // Fetch give-up suppression decays here rather than inside any one layer, so a
        // hunter that gave up on an unreachable gun comes back to the idea at the same
        // rate under `AI=pd`, the utility scorer and the FSM alike.
        self.fetch_block = (self.fetch_block - dt).max(0.0);

        let mut step = EnemyStep::default();
        // ── `AI=pd`: Perfect Dark's action ladder, ahead of both of ours ──
        // Neither the utility scorer nor the FSM runs; see `pd_step` for the ladder and
        // for what PD has instead of each thing it skips.
        if tuning.mode.is_pd() {
            self.pd_step(&mut step, dt, target, band, tuning, nav, physics, fire_anim);
            step.caught = self.catches(player_feet);
            return step;
        }
        // Utility-AI decision layer (roadmap #4, default ON via the World flag): a scored
        // behaviour selector replaces the hand-coded FSM transitions below. The FSM is
        // kept verbatim as the `utility == false` kill-switch.
        if self.utility {
            self.util_step(
                &mut step, dt, target, standoff, attack_range, tuning, nav,
                physics, fire_anim, self_collider, perceived,
            );
            step.caught = self.catches(player_feet);
            return step;
        }
        // Acquisition gate for the three BLIND states. An omniscient hunter is never
        // blind — PD's `bot_choose_general_target` always hands it a target, so it goes
        // straight into the engagement chain and never sweeps for you.
        let acquired = perceived || self.omniscient;
        // Shopping outranks the whole engagement chain, whatever state we were in — the
        // FSM's copy of the dominant `Fetch` score. The kill-switch keeps the behaviour
        // rather than degrading, because "hunters charge you bare-handed" is exactly the
        // symptom the switch would be flipped to rule out.
        if self.wants_fetch() {
            self.state = AiState::Fetch;
        } else if self.state == AiState::Fetch {
            // Got what it came for (or gave up): rejoin the sweep and let the ordinary
            // transitions take over from there.
            self.state = AiState::Search;
            self.search_target = None;
        }
        match self.state {
            AiState::Fetch => self.fetch_step(dt, nav, SPEED_FETCH * tuning.speed_mult),
            AiState::Idle => {
                // Unaware and with nowhere assigned to search — the `World` will give
                // it a point (spawn-in / stuck fallback). Acquire on sight meanwhile.
                if acquired {
                    self.enter_alert();
                } else {
                    step.needs_search_target = true;
                }
            }
            AiState::Search => {
                if acquired {
                    self.enter_alert();
                } else {
                    match self.search_target {
                        Some(t) => {
                            if self.move_toward(dt, t, nav, SPEED_SEARCH) {
                                // Reached it (or it's unreachable) — ask for the next.
                                self.search_target = None;
                                step.needs_search_target = true;
                            }
                        }
                        None => step.needs_search_target = true,
                    }
                }
            }
            AiState::Investigate => {
                if acquired {
                    self.enter_alert();
                } else {
                    match self.last_known {
                        // Still walking to the spot we're curious about.
                        Some(t) if self.dist_to(t) > ARRIVE_DIST => {
                            self.move_toward(dt, t, nav, SPEED_SEARCH);
                        }
                        // Arrived (or nothing to walk to): scan around, sweeping the
                        // cone, then give up to a fresh search.
                        _ => {
                            self.scan_timer += dt;
                            self.sweep_heading(dt);
                            if self.scan_timer >= INVESTIGATE_SCAN_DURATION {
                                self.state = AiState::Search;
                                self.last_known = None;
                                self.search_target = None;
                                step.needs_search_target = true;
                            }
                        }
                    }
                }
            }
            AiState::Alert => {
                self.face(target_pos);
                self.alert_timer += dt;
                if self.alert_timer >= tuning.alert {
                    self.state = AiState::Chase;
                    self.chase_timer = 0.0;
                }
            }
            AiState::Chase => {
                let dist = self.engage_dist(target_pos);
                let los = self.can_see(target, physics);
                // `!fire_anim` is deliberately NOT a condition here — see the matching
                // note in `util_score`. A Perfect Dark simulant fires near-continuously,
                // so gating the Attack entry on "no burst in flight" left it in `Chase`
                // forever, running the player down to point-blank while shooting.
                if dist <= attack_range && los {
                    self.face(target_pos);
                    self.enter_attack();
                    self.path.clear();
                } else {
                    // Suppressing fire while closing (difficulty-gated): if we can see
                    // the player and they're inside the suppress band — which has zero
                    // width at difficulty 0, so the baseline "close, THEN fire" is
                    // unchanged — lay down fire on the move instead of holding it until
                    // we reach standoff. Firing is a timer, so the legs keep running on
                    // locomotion and the chest-aim still points the gun at the player.
                    // We slow from a run to a jog while a suppress burst is in flight so
                    // it reads as a deliberate advancing volley, not a panicked sprint.
                    let suppress_range = attack_range + SUPPRESS_BAND * tuning.suppress;
                    let firing = fire_anim || self.is_attacking;
                    if tuning.suppress > 0.0 && los && dist <= suppress_range {
                        if !firing {
                            self.is_attacking = true;
                            self.fire_started = false;
                            step.want_fire = true;
                        }
                        if self.is_attacking && !self.fire_started && fire_anim {
                            self.fire_started = true;
                        }
                        // Burst finished → allow the next one (no Cooldown/reposition
                        // in Chase; the burst window itself paces the volleys).
                        if self.is_attacking && self.fire_started && !fire_anim {
                            self.is_attacking = false;
                        }
                    }
                    let chase_speed = if firing { SPEED_ADVANCE } else { SPEED_CHASE };
                    // Path to where we last saw the player (updated to the live
                    // position every perceived step above). Reaching that spot without
                    // seeing them = they got away → investigate it. A burst in flight
                    // no longer freezes movement — firing is a timer, and the legs run
                    // on locomotion while the arm keeps its procedural aim.
                    //
                    // Flanking (#3, difficulty-gated): instead of running dead-straight
                    // at the spot, curve in from an offset bearing on this hunter's
                    // assigned side — 0 swing at difficulty 0 (unchanged straight chase),
                    // wider with `flank`, and packmates split sides so the pack surrounds.
                    let base = self.known_target_pos(target_pos).unwrap_or(target_pos);
                    let flank_angle = FLANK_MAX_ANGLE * tuning.flank * self.flank_dir;
                    let target = flank_point(base, self.pos, flank_angle);
                    // Arriving at the believed spot without eyes on = they got away →
                    // go poke at it. `lose_contact` keeps an omniscient hunter chasing
                    // (its "believed spot" is the live one, so there is nothing to poke).
                    if self.move_toward(dt, target, nav, chase_speed * tuning.speed_mult) && !perceived {
                        self.lose_contact();
                    }
                }
            }
            AiState::Attack => {
                let dist = self.engage_dist(target_pos);
                let los = self.can_see(target, physics);
                // Debounce the LOS bail: only a *sustained* loss ([`ATTACK_LOS_GRACE`])
                // drops us to Chase, so a one-frame corner-seam flicker doesn't thrash
                // Attack↔Chase (the ORCA-exposed jank the lab caught).
                if los {
                    self.attack_los_lost = 0.0;
                } else {
                    self.attack_los_lost += dt;
                }
                if dist > attack_range * 1.3 || self.attack_los_lost >= ATTACK_LOS_GRACE {
                    self.state = AiState::Chase;
                    self.chase_timer = 0.0;
                    self.is_attacking = false;
                    self.holding = false;
                } else {
                    // Advance / hold / back off around the standoff, with a hysteresis
                    // dead-band so the feet don't micro-step at the boundary. Too far →
                    // jog in; too close → give ground (the player, or a packmate's
                    // separation-nudge, can otherwise shove a hunter to point-blank —
                    // enemies don't collide with the player, so nothing else stops it);
                    // inside the band → plant and hold. Always face the player so the
                    // procedural aim points the gun at them (backpedal look while
                    // retreating).
                    if !los {
                        // **No sight line → close, whatever the distance says.** PD's
                        // rule, stated twice in `botcmd_tick_dist_mode` (`botcmd.c:135`
                        // and `:163`): an out-of-sight target turns both `OK` (hold) and
                        // `BACKUP` (give ground) into `ADVANCE`. Without it a hunter
                        // whose sightline flickers — heads bobbing over a floor edge,
                        // a corner seam — sits inside its standoff band forever,
                        // holding a distance it has no shot along, because
                        // `ATTACK_LOS_GRACE` keeps resetting before it can bail.
                        self.holding = false;
                        self.move_toward(dt, target_pos, nav, SPEED_ADVANCE * tuning.speed_mult);
                    } else if self.holding {
                        // Leave the hold only when the player pulls clearly outside the
                        // band on either side.
                        if !(standoff - STANDOFF_HYST..=standoff + STANDOFF_HYST).contains(&dist) {
                            self.holding = false;
                        }
                    } else if dist > standoff {
                        self.move_toward(dt, target_pos, nav, SPEED_ADVANCE * tuning.speed_mult); // jog in
                    } else if dist < standoff - STANDOFF_HYST {
                        self.back_off(dt, target_pos, nav, SPEED_ADVANCE * tuning.speed_mult); // give ground
                    } else {
                        self.holding = true;
                    }
                    // Evasion: ONLY the reactive aim-dodge (the passive constant weave
                    // was removed so a sidestep reads as a reaction to the player's aim,
                    // not general jitter). Layered on the in/out standoff logic above;
                    // facing is re-set to the player right after so the gun stays on
                    // target while the body jukes off the shot line.
                    if self.evade_burst > 0.0 {
                        self.evade_step(dt, target_pos, nav);
                    }
                    self.face(target_pos);
                    // Request a fire burst once per attack entry.
                    if !fire_anim && !self.is_attacking {
                        self.is_attacking = true;
                        self.fire_started = false;
                        step.want_fire = true;
                    }
                    if self.is_attacking && !self.fire_started && fire_anim {
                        self.fire_started = true;
                    }
                    // Fire animation finished → break off for the next volley. At
                    // higher difficulty (or whenever hurt) the hunter ducks to a
                    // no-LOS cell and peek-fires from it (#1/#4) — but only if real
                    // cover exists nearby; otherwise it falls back to the open
                    // burst-and-reposition juke (the baseline).
                    if self.is_attacking && self.fire_started && !fire_anim {
                        self.is_attacking = false;
                        let want_cover = tuning.cover > 0.0
                            && (self.is_hurt() || tuning.cover >= COVER_UNHURT_MIN);
                        let cover = if want_cover {
                            self.sample_cover_cell(self.pos, target_pos, nav, physics, self_collider, false)
                        } else {
                            None
                        };
                        match cover {
                            Some(spot) => {
                                self.cover_target = Some(spot);
                                self.peek_target = None;
                                self.cover_timer = 0.0;
                                self.state = AiState::TakeCover;
                            }
                            None => {
                                // Flip the weave direction first so successive bursts
                                // alternate sides, then pick the open reposition spot.
                                self.strafe_dir = -self.strafe_dir;
                                self.enter_cooldown_reposition(
                                    target_pos, standoff, nav, physics, self_collider,
                                );
                            }
                        }
                    }
                }
            }
            AiState::Cooldown => {
                self.cooldown_timer += dt;
                // Burst-and-reposition: juke to the new firing angle while the
                // cooldown runs, facing the travel direction so the legs read
                // cleanly (no strafe clip). Fall back to holding + facing the player
                // if there was no spot to juke to.
                let arrived = match self.reposition_target {
                    Some(t) => self.move_toward(dt, t, nav, REPOSITION_SPEED * tuning.speed_mult),
                    None => {
                        self.face(target_pos);
                        true
                    }
                };
                // Arrived at the new spot (or the cooldown elapsed) → plant, face the
                // player, and re-evaluate the engagement exactly as before.
                if arrived || self.cooldown_timer >= tuning.cooldown {
                    self.reposition_target = None;
                    self.face(target_pos);
                    let dist = self.engage_dist(target_pos);
                    let los = self.can_see(target, physics);
                    if dist <= attack_range && los {
                        self.enter_attack();
                    } else if dist <= perception {
                        self.state = AiState::Chase;
                        self.chase_timer = 0.0;
                    } else {
                        // Lost them — go poke at where they last were (an omniscient
                        // hunter just keeps closing; see `lose_contact`).
                        self.lose_contact();
                    }
                }
            }
            AiState::TakeCover => {
                // Relocate to the chosen hidden cell (jog — a deliberate break, not a
                // panic sprint). We're "in cover" once we arrive OR line-of-sight to
                // the player is already broken (a wall came between us en route).
                let arrived = match self.cover_target {
                    Some(t) => self.move_toward(dt, t, nav, SPEED_ADVANCE * tuning.speed_mult),
                    None => true,
                };
                let hidden = !(self.can_see(target, physics));
                if arrived || hidden {
                    self.face(target_pos); // ready to pop back out
                    self.cover_timer += dt;
                    let dwell = COVER_DWELL_LO + (COVER_DWELL_HI - COVER_DWELL_LO) * tuning.cover;
                    if self.cover_timer >= dwell {
                        // Peek: find a nearby cell that DOES see the player and pop out
                        // to it. If none exists (they moved so nothing sees them), give
                        // up cover — re-engage if we can still perceive them, else go
                        // investigate where they were.
                        let base = self.cover_target.unwrap_or(self.pos);
                        self.peek_target =
                            self.sample_cover_cell(base, target_pos, nav, physics, self_collider, true);
                        if self.peek_target.is_some() {
                            self.state = AiState::Peek;
                            self.is_attacking = false;
                            self.fire_started = false;
                        } else if perceived {
                            self.enter_attack();
                        } else {
                            self.lose_contact();
                        }
                    }
                }
            }
            AiState::Peek => {
                // Step out to the pop-out cell, then fire one burst and duck back.
                let arrived = match self.peek_target {
                    Some(t) => self.move_toward(dt, t, nav, SPEED_ADVANCE * tuning.speed_mult),
                    None => true,
                };
                let los = self.can_see(target, physics);
                if arrived {
                    // Keep dodging while exposed at the peek: if the player has a bead
                    // on us, juke off the shot line (same reactive evade as Attack), so
                    // a peeking hunter doesn't stand still to be shot.
                    if self.evade_burst > 0.0 {
                        self.evade_step(dt, target_pos, nav);
                    }
                    self.face(target_pos);
                    if los {
                        // Pop-out volley: one burst on the same want_fire/fire_started
                        // lifecycle as Attack, then duck back to fresh cover.
                        if !fire_anim && !self.is_attacking {
                            self.is_attacking = true;
                            self.fire_started = false;
                            step.want_fire = true;
                        }
                        if self.is_attacking && !self.fire_started && fire_anim {
                            self.fire_started = true;
                        }
                        if self.is_attacking && self.fire_started && !fire_anim {
                            self.is_attacking = false;
                            self.duck_to_cover(target_pos, nav, physics, self_collider);
                        }
                    } else {
                        // Popped out but the player isn't there any more — re-engage if
                        // still perceived, else investigate the LKP.
                        if perceived {
                            self.enter_attack();
                        } else {
                            self.is_attacking = false;
                            self.holding = false;
                            self.lose_contact();
                        }
                        self.scan_timer = 0.0;
                    }
                }
            }
        }

        step.caught = self.catches(player_feet);
        step
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Utility-AI decision layer (roadmap #4). A scored behaviour selector replaces
    // the FSM's special-cased transitions: each tick every candidate behaviour is
    // scored from the context, the winner is committed (with inertia so it doesn't
    // thrash), and it executes via the SAME movement/fire/cover helpers the FSM uses.
    // The six behaviours are now composable scored options — a 7th is a new score
    // entry, not another flag threaded through `update`. The sequential combat sub-
    // cycle (TakeCover→Peek, Cooldown reposition) runs as a COMMITTED behaviour that
    // completes and hands back to the scorer.
    // ─────────────────────────────────────────────────────────────────────────

    /// The fire-burst lifecycle pump (shared with the FSM's inline version): request a
    /// burst when idle, latch its start, and detect its completion. Returns `true` on the
    /// tick a burst just ended.
    fn pump_fire(&mut self, fire_anim: bool, step: &mut EnemyStep) -> bool {
        if !fire_anim && !self.is_attacking {
            self.is_attacking = true;
            self.fire_started = false;
            step.want_fire = true;
        }
        if self.is_attacking && !self.fire_started && fire_anim {
            self.fire_started = true;
        }
        if self.is_attacking && self.fire_started && !fire_anim {
            self.is_attacking = false;
            return true;
        }
        false
    }

    /// Utility score for candidate behaviour `s` in the current context (higher = more
    /// desirable). These reproduce the FSM's logic as continuous, composable scores;
    /// `util_choose` adds inertia + picks the max. `los_hold` is LOS debounced by the
    /// attack grace so a corner-seam flicker doesn't drop the fight.
    fn util_score(
        &self,
        s: AiState,
        perceived: bool,
        engaged: bool,
        dist: f32,
        los_hold: bool,
        attack_range: f32,
        tuning: AiTuning,
    ) -> f32 {
        match s {
            // Baseline floor — only wins when nothing else applies (blind, nowhere to go).
            AiState::Idle => 0.1,
            // **Cannot shoot → go and get something to shoot with.** Dominant by design
            // (see [`FETCH_SCORE`]): it is not weighed against fighting, because a hunter
            // holding nothing has nothing to weigh. Zero the instant the `World` stops
            // handing it a fetch point, which is the instant it can fight again.
            AiState::Fetch => {
                if self.wants_fetch() {
                    FETCH_SCORE
                } else {
                    0.0
                }
            }
            AiState::Search => {
                if !engaged && !perceived && self.last_known.is_none() {
                    0.6
                } else {
                    0.0
                }
            }
            AiState::Investigate => {
                if !engaged && !perceived && self.last_known.is_some() {
                    0.7
                } else {
                    0.0
                }
            }
            // Serve the acquisition reaction delay before pressing the attack.
            AiState::Alert => {
                if (perceived || engaged) && !self.alert_served {
                    1.2
                } else {
                    0.0
                }
            }
            // Close on / pursue the last-known once the alert is served. Always a decent
            // option while engaged; Attack outscores it when in range with eyes on.
            AiState::Chase => {
                if (perceived || engaged) && self.alert_served {
                    0.7
                } else {
                    0.0
                }
            }
            // Plant + fire: high inside the standoff band with LOS; held out to 1.3× once
            // already attacking (the exit hysteresis) so the player must clearly break off.
            AiState::Attack => {
                if self.alert_served && los_hold {
                    if dist <= attack_range {
                        // Clearly beats Chase(0.7)+inertia so a closing hunter hands off
                        // to the standoff hold (else it runs the player to point-blank).
                        //
                        // **A burst in flight is deliberately NOT an input here.** It used
                        // to zero this score — "don't initiate the plant mid-suppress-burst,
                        // so the pack pushes through chokepoints instead of stopping to
                        // shoot the instant it's barely in range". That was written when
                        // only the FSM pulled the trigger, so a burst meant "mid-approach
                        // volley" and came in short bursts with gaps to plant in. The
                        // Perfect Dark model owns the trigger now and fires near-continuously
                        // at the top tiers, so the exemption never lifted: `Chase` (0.7) won
                        // forever and the hunter ran the player down to point-blank, firing.
                        //
                        // Nothing is lost by dropping it, because `Attack` advances to its
                        // standoff on its own (see the Attack arm) — entering it early does
                        // not stop the hunter, it just closes at a jog instead of a run. And
                        // it matches `botcmd_tick_dist_mode`, where whether the bot is
                        // shooting is not an input to the distance mode at all.
                        1.0
                    } else if dist <= attack_range * 1.3 && self.state == AiState::Attack {
                        0.9 // exit hysteresis: hold out to 1.3× once already attacking
                    } else {
                        0.0
                    }
                } else {
                    0.0
                }
            }
            // Between-bursts moves — only viable on the post-burst edge. Break to cover if
            // hurt or cover-tuned; otherwise reposition. (Peek is reached only from the
            // committed cover cycle, never scored fresh.)
            AiState::TakeCover => {
                if self.post_burst
                    && tuning.cover > 0.0
                    && (self.is_hurt() || tuning.cover >= COVER_UNHURT_MIN)
                {
                    1.5
                } else {
                    0.0
                }
            }
            AiState::Cooldown => {
                if self.post_burst {
                    1.3
                } else {
                    0.0
                }
            }
            AiState::Peek => 0.0,
        }
    }

    /// Pick the highest-scoring behaviour, with an inertia bonus to the current one so a
    /// near-tie at a band boundary sticks (decision hysteresis).
    fn util_choose(
        &self,
        perceived: bool,
        engaged: bool,
        dist: f32,
        los_hold: bool,
        attack_range: f32,
        tuning: AiTuning,
    ) -> AiState {
        const CANDIDATES: [AiState; 9] = [
            AiState::Idle,
            AiState::Fetch,
            AiState::Search,
            AiState::Investigate,
            AiState::Alert,
            AiState::Chase,
            AiState::Attack,
            AiState::TakeCover,
            AiState::Cooldown,
        ];
        let mut best = (AiState::Idle, f32::MIN);
        for s in CANDIDATES {
            let mut sc = self.util_score(s, perceived, engaged, dist, los_hold, attack_range, tuning);
            if s == self.state {
                sc += UTIL_INERTIA;
            }
            if sc > best.1 {
                best = (s, sc);
            }
        }
        best.0
    }

    /// Enter-hook for a behaviour change: sample cover / pick a reposition / reset the
    /// per-behaviour timers, mirroring the FSM's state-entry side effects. Sets
    /// `self.state` to the chosen behaviour (or to `Cooldown` if a cover break found no
    /// cover — the FSM's fallback).
    fn util_enter(
        &mut self,
        next: AiState,
        target: EngageTarget,
        standoff: f32,
        nav: &NavWorld,
        physics: &mut PhysicsWorld,
        self_collider: ColliderHandle,
    ) {
        let target_pos = target.pos;
        self.state = next;
        match next {
            AiState::Alert => {
                self.alert_timer = 0.0;
                self.path.clear();
            }
            AiState::Chase => self.chase_timer = 0.0,
            AiState::Attack => self.enter_attack(),
            AiState::Investigate => self.scan_timer = 0.0,
            // Break off whatever it was walking toward and plan a fresh path to the
            // pickup — a hunter that ran dry mid-chase is holding a path to the player.
            AiState::Fetch => {
                self.path.clear();
                self.repath_timer = 0.0;
                self.holding = false;
                self.is_attacking = false;
            }
            AiState::TakeCover => {
                match self.sample_cover_cell(self.pos, target_pos, nav, physics, self_collider, false) {
                    Some(spot) => {
                        self.cover_target = Some(spot);
                        self.peek_target = None;
                        self.cover_timer = 0.0;
                    }
                    None => {
                        // No cover → the FSM's fallback: flip the weave + open reposition.
                        self.strafe_dir = -self.strafe_dir;
                        self.enter_cooldown_reposition(target_pos, standoff, nav, physics, self_collider);
                    }
                }
            }
            AiState::Cooldown => {
                self.strafe_dir = -self.strafe_dir;
                self.enter_cooldown_reposition(target_pos, standoff, nav, physics, self_collider);
            }
            _ => {}
        }
    }

    /// One utility tick: score → commit (with inertia) → enter-hook on a change →
    /// execute via the shared helpers. See the section header above.
    #[allow(clippy::too_many_arguments)]
    fn util_step(
        &mut self,
        step: &mut EnemyStep,
        dt: f32,
        target: EngageTarget,
        standoff: f32,
        attack_range: f32,
        tuning: AiTuning,
        nav: &NavWorld,
        physics: &mut PhysicsWorld,
        fire_anim: bool,
        self_collider: ColliderHandle,
        perceived: bool,
    ) {
        let target_pos = target.pos;
        let dist = self.engage_dist(target_pos);
        let has_los = self.can_see(target, physics);
        // Debounced LOS for the attack hysteresis (a one-frame flicker mustn't drop it).
        if has_los {
            self.attack_los_lost = 0.0;
        } else {
            self.attack_los_lost += dt;
        }
        let los_hold = has_los || self.attack_los_lost < ATTACK_LOS_GRACE;
        // Belief: "engaged" while a fresh-enough last-known is held. An OMNISCIENT
        // hunter's knowledge never ages, so it is permanently engaged — which is what
        // zeroes the `Search` / `Investigate` scores (both require `!engaged`) and
        // makes those behaviours unreachable for it without a special case.
        let engaged = self.omniscient
            || (self.last_known.is_some() && self.since_seen < ENGAGE_MEMORY);
        // The sequential combat sub-cycle is COMMITTED once entered — it runs to
        // completion in its executor and hands back to the scorer — so the scorer only
        // governs the reactive choice + INITIATING the between-bursts move (post_burst).
        let committed = matches!(
            self.state,
            AiState::TakeCover | AiState::Peek | AiState::Cooldown
        ) && !self.post_burst;
        let next = if committed {
            self.state
        } else {
            self.util_choose(perceived, engaged, dist, los_hold, attack_range, tuning)
        };
        if next != self.state {
            self.util_enter(next, target, standoff, nav, physics, self_collider);
        }
        self.post_burst = false; // consume the edge (may be re-armed by Attack below)

        // ── Execute the committed behaviour (`self.state`, which `util_enter` may have
        // redirected TakeCover→Cooldown). Movement/fire come from the shared helpers.
        match self.state {
            AiState::Fetch => self.fetch_step(dt, nav, SPEED_FETCH * tuning.speed_mult),
            AiState::Idle => {
                if !perceived {
                    step.needs_search_target = true;
                }
            }
            AiState::Search => match self.search_target {
                Some(t) => {
                    if self.move_toward(dt, t, nav, SPEED_SEARCH) {
                        self.search_target = None;
                        step.needs_search_target = true;
                    }
                }
                None => step.needs_search_target = true,
            },
            AiState::Investigate => match self.known_target_pos(target_pos) {
                Some(t) if self.dist_to(t) > ARRIVE_DIST => {
                    self.move_toward(dt, t, nav, SPEED_SEARCH);
                }
                _ => {
                    self.scan_timer += dt;
                    self.sweep_heading(dt);
                    if self.scan_timer >= INVESTIGATE_SCAN_DURATION {
                        self.last_known = None;
                        self.search_target = None;
                        step.needs_search_target = true;
                    }
                }
            },
            AiState::Alert => {
                self.face(target_pos);
                self.alert_timer += dt;
                if self.alert_timer >= tuning.alert {
                    self.alert_served = true;
                }
            }
            AiState::Chase => {
                // Suppressing fire while closing (difficulty-gated), then advance along
                // the (optionally flanked) bearing toward the last-known spot.
                let suppress_range = attack_range + SUPPRESS_BAND * tuning.suppress;
                let firing = fire_anim || self.is_attacking;
                if tuning.suppress > 0.0 && has_los && dist <= suppress_range {
                    self.pump_fire(fire_anim, step); // Chase bursts just cycle (no reposition)
                }
                let chase_speed = if firing { SPEED_ADVANCE } else { SPEED_CHASE };
                let base = self.known_target_pos(target_pos).unwrap_or(target_pos);
                let flank_angle = FLANK_MAX_ANGLE * tuning.flank * self.flank_dir;
                let target = flank_point(base, self.pos, flank_angle);
                self.move_toward(dt, target, nav, chase_speed * tuning.speed_mult);
            }
            AiState::Attack => {
                // Standoff in/out/hold with a hysteresis dead-band, reactive evade, and
                // the fire pump; a finished burst arms `post_burst` so next tick's scorer
                // breaks to cover / repositions.
                // Out of sight → close, whatever the distance says (see the FSM arm).
                if !has_los {
                    self.holding = false;
                    self.move_toward(dt, target_pos, nav, SPEED_ADVANCE * tuning.speed_mult);
                } else if self.holding {
                    if !(standoff - STANDOFF_HYST..=standoff + STANDOFF_HYST).contains(&dist) {
                        self.holding = false;
                    }
                } else if dist > standoff {
                    self.move_toward(dt, target_pos, nav, SPEED_ADVANCE * tuning.speed_mult);
                } else if dist < standoff - STANDOFF_HYST {
                    self.back_off(dt, target_pos, nav, SPEED_ADVANCE * tuning.speed_mult);
                } else {
                    self.holding = true;
                }
                if self.evade_burst > 0.0 {
                    self.evade_step(dt, target_pos, nav);
                }
                self.face(target_pos);
                if self.pump_fire(fire_anim, step) {
                    self.post_burst = true;
                }
            }
            AiState::Cooldown => {
                self.cooldown_timer += dt;
                let arrived = match self.reposition_target {
                    Some(t) => self.move_toward(dt, t, nav, REPOSITION_SPEED * tuning.speed_mult),
                    None => {
                        self.face(target_pos);
                        true
                    }
                };
                if arrived || self.cooldown_timer >= tuning.cooldown {
                    self.reposition_target = None;
                    self.face(target_pos);
                    // Leave the committed set → the scorer re-evaluates next tick.
                    self.state = AiState::Chase;
                }
            }
            AiState::TakeCover => {
                let arrived = match self.cover_target {
                    Some(t) => self.move_toward(dt, t, nav, SPEED_ADVANCE * tuning.speed_mult),
                    None => true,
                };
                let hidden = !(self.can_see(target, physics));
                if arrived || hidden {
                    self.face(target_pos);
                    self.cover_timer += dt;
                    let dwell = COVER_DWELL_LO + (COVER_DWELL_HI - COVER_DWELL_LO) * tuning.cover;
                    if self.cover_timer >= dwell {
                        let base = self.cover_target.unwrap_or(self.pos);
                        self.peek_target =
                            self.sample_cover_cell(base, target_pos, nav, physics, self_collider, true);
                        if self.peek_target.is_some() {
                            self.state = AiState::Peek;
                            self.is_attacking = false;
                            self.fire_started = false;
                        } else {
                            // Nothing to peek to → leave the cycle; the scorer re-decides.
                            self.state = AiState::Chase;
                        }
                    }
                }
            }
            AiState::Peek => {
                let arrived = match self.peek_target {
                    Some(t) => self.move_toward(dt, t, nav, SPEED_ADVANCE * tuning.speed_mult),
                    None => true,
                };
                let los = self.can_see(target, physics);
                if arrived {
                    if self.evade_burst > 0.0 {
                        self.evade_step(dt, target_pos, nav);
                    }
                    self.face(target_pos);
                    if los {
                        if self.pump_fire(fire_anim, step) {
                            self.duck_to_cover(target_pos, nav, physics, self_collider);
                        }
                    } else {
                        // Popped out but they're gone → leave the cycle; scorer re-decides.
                        self.state = AiState::Chase;
                        self.is_attacking = false;
                        self.holding = false;
                    }
                }
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Perfect Dark's action ladder (`AI=pd`). `bot_tick_unpaused` (`bot.c:2445`)
    // runs, each frame and in this order:
    //
    //   1. reload check ......... ported — but it lives in the `World`, which owns the
    //                             magazine and the trigger (`EnemyInstance::reload`).
    //   2. weapon switch ........ skipped: a hunter carries one gun and no inventory.
    //   3. cloak / RCP120 ....... skipped: neither exists here.
    //   4. scenario command ..... skipped: CTF / KOTH / Hill are PD game modes.
    //   5. ATTACK THE TARGET .... this function — `botcmd_tick_dist_mode` over our nav.
    //   6. follow a teammate .... unreachable, and faithfully so: PD falls to this rung
    //                             only when `bot_choose_general_target` returns nothing,
    //                             and with omniscience on there is always a target (the
    //                             player, if a chosen packmate dies). Left unwritten
    //                             rather than written and dead.
    //   7. pick up a weapon ..... ported (`bot_pick_up_weapon`) — see the fetch rung at
    //                             the top of this function. The port note here used to
    //                             read "no pickups in this game"; there are now, and a
    //                             bot that cannot shoot has nothing to run rung 5 with.
    //
    // What is NOT in that ladder is as load-bearing as what is: no search, no
    // investigate, no reaction-delay state, no cover, no strafe. A PD bot's answer to
    // "the player is aiming at me" is to keep shooting.
    // ─────────────────────────────────────────────────────────────────────────

    /// One tick of the Perfect Dark ladder: decide a distance mode, execute it over
    /// the nav grid, and pull the trigger while the target is in sight.
    ///
    /// **The pathing is ours, the decisions are PD's.** Perfect Dark moves bots along
    /// hand-authored pads and waypoint lists that our levels do not have, so
    /// `chr_go_to_prop` becomes [`Self::move_toward`] (beeline where the line is clear,
    /// grid A* where it is not) and `chr_run_from_pos` becomes [`Self::back_off`].
    /// `DistDecision::reissue` is honoured by dropping the current path, which is what
    /// re-issuing a movement command means here; between re-issues the hunter walks the
    /// path it already has, exactly as PD's 1 s `distmodettl60` intends.
    #[allow(clippy::too_many_arguments)]
    fn pd_step(
        &mut self,
        step: &mut EnemyStep,
        dt: f32,
        target: EngageTarget,
        band: DistBand,
        tuning: AiTuning,
        nav: &NavWorld,
        physics: &mut PhysicsWorld,
        fire_anim: bool,
    ) {
        let target_pos = target.pos;
        // ── Rung 7, hoisted: pick up a weapon ──
        // PD runs `bot_pick_up_weapon` *below* attacking, and for a bot with a working
        // gun the order is invisible either way. For one that cannot shoot it is the
        // whole behaviour: rung 5 would walk it into the target's face to pull a trigger
        // that does nothing. So a hunter that wants something goes and gets it first, and
        // the deathmatch floor economy reaches `AI=pd` instead of being switched off for
        // it (the alternative on the table was arming PD hunters at spawn).
        if self.wants_fetch() {
            self.state = AiState::Fetch;
            self.fetch_step(dt, nav, SPEED_CHASE * tuning.speed_mult); // every PD move runs
            self.last_known = Some(target_pos); // a PD bot's knowledge never goes stale
            return;
        }
        // `aibot->targetinsight` is a raw line of sight, not a view cone: PD's 45° cone
        // gates *firing* (in `pdsim`), never seeing. So this deliberately does not go
        // through `perceives`.
        let insight = self.can_see(target, physics);
        // `chr_get_distance_to_coord` (`botcmd.c:98`) — the 3D separation, so a target
        // one floor up is not "at the right distance".
        let dist = self.engage_dist(target_pos);
        // `chr->actiontype == ACT_STAND`: the feet are not currently travelling.
        let standing = self.vel.length_squared() < 0.01;
        let roll = self.rand01();
        let decision = self.dist.tick(dist, band, insight, tuning.tier, standing, dt, roll);
        if decision.reissue {
            // Re-issue the movement command: drop the path in hand so the next
            // `move_toward` plans a fresh one against the target's live position.
            self.path.clear();
            self.repath_timer = 0.0;
        }

        let speed = SPEED_CHASE * tuning.speed_mult; // every PD mode uses GOPOSFLAG_RUN
        match decision.mode {
            // `chr_run_from_pos(chr, GOPOSFLAG_RUN, 10000, &targetprop->pos)`.
            DistMode::Backup => {
                self.back_off(dt, target_pos, nav, speed);
                self.face(target_pos);
            }
            // `chr_try_stop(chr)` — plant and shoot. This is the single most visible
            // difference from our AI, which weaves between bursts and never stops.
            DistMode::Ok => {
                self.face(target_pos);
            }
            // `chr_go_to_prop(chr, targetprop, GOPOSFLAG_RUN)` — both modes, identically.
            DistMode::Advance | DistMode::Goto => {
                self.move_toward(dt, target_pos, nav, speed);
            }
        }
        self.holding = decision.mode == DistMode::Ok;

        // Pull the trigger whenever the target is in sight. In the shipping game the
        // simulant owns this decision outright (`World::step` prefers its `want_fire`,
        // which fires while still converging); this keeps a `pdsim`-less headless hunter
        // behaving like a bot rather than like a statue.
        if insight {
            self.pump_fire(fire_anim, step);
        } else if self.is_attacking && !fire_anim {
            self.is_attacking = false;
        }

        // Report a state so the HUD, the squad broadcaster and the AI lab keep working.
        // `Ok`/`Backup` are combat at distance (Attack); `Advance`/`Goto` are the run-in
        // (Chase). The blind states are unreachable here — which is the point.
        self.state = match decision.mode {
            DistMode::Ok | DistMode::Backup => AiState::Attack,
            DistMode::Advance | DistMode::Goto => AiState::Chase,
        };
        self.last_known = Some(target_pos); // a PD bot's knowledge never goes stale
    }

    /// Walk to the fetch point and stand on it until the `World` hands over the goods.
    ///
    /// **One executor, three decision layers** (the utility scorer, the legacy FSM and
    /// Perfect Dark's ladder), which is the lesson from the last attempt: the fetch
    /// destination was reachable from exactly one of them, so the default layer steered
    /// by something else entirely and nobody noticed.
    ///
    /// Arrival means "hold here": the collect is the `World`'s (`hunter_pickup_step`,
    /// on a radius wider than [`ARRIVE_DIST`]), so there is nothing to do but be still
    /// and let it land. [`FETCH_STALL`] is the guard against holding forever at
    /// something unreachable.
    fn fetch_step(&mut self, dt: f32, nav: &NavWorld, speed: f32) {
        let Some(to) = self.fetch_target else {
            self.fetch_stall = 0.0;
            return;
        };
        // `move_toward` reports `true` for arrived AND for unreachable — both mean "no
        // more walking will help", which is exactly what the give-up clock times.
        if self.move_toward(dt, to, nav, speed) {
            self.face(to);
            self.fetch_stall += dt;
            if self.fetch_stall >= FETCH_STALL {
                self.fetch_stall = 0.0;
                self.fetch_block = FETCH_RETRY;
                self.path.clear();
            }
        } else {
            self.fetch_stall = 0.0;
        }
    }

    /// Begin the reaction delay after acquiring the player.
    fn enter_alert(&mut self) {
        self.state = AiState::Alert;
        self.alert_timer = 0.0;
        self.path.clear();
    }

    /// Enter `Attack`: reset the fire lifecycle, the standoff-hold hysteresis, and the
    /// LOS-loss grace (so a stale grace from a previous engagement can't bail us out on
    /// the first flickering frame). Every Attack transition goes through here.
    fn enter_attack(&mut self) {
        self.state = AiState::Attack;
        self.is_attacking = false;
        self.holding = false;
        self.attack_los_lost = 0.0;
    }

    /// Rotate the facing in place (used to scan a spot in `Investigate`), so the
    /// perception cone sweeps and can re-acquire the player.
    fn sweep_heading(&mut self, dt: f32) {
        let ang = SCAN_TURN_RATE * dt;
        let (s, c) = ang.sin_cos();
        let (x, z) = (self.heading.x, self.heading.z);
        let h = Vec3::new(x * c - z * s, 0.0, x * s + z * c);
        if h.length_squared() > 1e-6 {
            self.heading = h.normalize();
        }
    }

    /// Move toward a flat `target` this step; returns `true` when the hunter has
    /// arrived (within [`ARRIVE_DIST`]) or the target is **unreachable** (no A* path
    /// and no clear line) so the caller can pick a new one instead of getting stuck.
    ///
    /// When the straight line to the target is walkable (an open room), **beeline** —
    /// move directly at any angle — so the hunter doesn't zig-zag along the grid's
    /// cardinal-only A* waypoints. Only when the line is blocked (a wall/corner) does
    /// it fall back to A* (the JS "LOS → beeline" shortcut). Shared by Chase, Search,
    /// and Investigate.
    /// Drop the hunter's feet to the standable floor beneath it (bounded, so a
    /// transient bad step can't teleport it across a full drop). Enemies have no
    /// gravity or collide-and-slide of their own — their Y comes only from the
    /// path they follow — so the flat beeline branch needs this to stay on the
    /// surface rather than floating over a rise.
    fn snap_to_floor(&mut self, nav: &NavWorld) {
        if let Some(fy) = nav.floor_height_at(self.pos.x, self.pos.z, self.pos.y + 0.25) {
            if (fy - self.pos.y).abs() <= 1.0 {
                self.pos.y = fy;
            }
        }
    }

    /// Give ground: step straight back from the player to regain the standoff. Called
    /// from `Attack` when the player has closed inside the standoff dead-band. Enemies
    /// don't collide with the player, so without this a rush (or a packmate's
    /// separation-nudge) could pin a hunter at point-blank and it would just hold and
    /// fire in your face. Only retreats where the step-back line is clear and lands on
    /// floor — it won't moonwalk through a wall or off a ledge (it just holds there
    /// instead, and the reposition juke will find a better angle next burst). The
    /// caller re-faces the player, so the model backpedals while covering you.
    fn back_off(&mut self, dt: f32, target_pos: Vec3, nav: &NavWorld, speed: f32) {
        let away = Vec3::new(self.pos.x - target_pos.x, 0.0, self.pos.z - target_pos.z);
        if away.length_squared() < 1e-6 {
            return; // standing on the player — no direction to back toward
        }
        let dir = away.normalize();
        let dest = self.pos + dir * (speed * dt);
        let up = Vec3::new(0.0, 0.5, 0.0);
        if nav.los_clear(self.pos + up, dest + up) && nav.ground_path_clear(self.pos, dest) {
            self.path.clear();
            self.repath_timer = 0.0; // force a fresh A* path when it re-engages
            // Preferred backpedal velocity; the caller re-faces the player so the model
            // covers you while giving ground. Committed by the `World`'s integrator.
            self.add_move(dir, speed);
        }
    }

    /// A committed reactive juke (player is aiming — see the evade trigger): a fast
    /// lateral burst in `evade_dir` at [`EVADE_SPEED`]. Flips the direction if blocked
    /// so it slides the other way rather than grinding the wall.
    fn evade_step(&mut self, dt: f32, target_pos: Vec3, nav: &NavWorld) {
        if !self.lateral_step(dt, target_pos, nav, self.evade_dir, EVADE_SPEED) {
            self.evade_dir = -self.evade_dir;
        }
    }

    /// Try to step laterally (perpendicular to the player bearing) by `dir` (+1/−1) at
    /// `speed` m/s this frame. Nav/LOS-gated like [`Self::back_off`] so it never clips
    /// a wall or walks off a ledge; returns `false` if the step was blocked (the caller
    /// flips its direction). Moves sideways only — roughly holding the standoff radius.
    fn lateral_step(&mut self, dt: f32, target_pos: Vec3, nav: &NavWorld, dir: f32, speed: f32) -> bool {
        let to = Vec3::new(target_pos.x - self.pos.x, 0.0, target_pos.z - self.pos.z);
        if to.length_squared() < 1e-6 {
            return false;
        }
        let fwd = to.normalize();
        let perp = Vec3::new(-fwd.z, 0.0, fwd.x) * dir;
        let dest = self.pos + perp * (speed * dt);
        let up = Vec3::new(0.0, 0.5, 0.0);
        if nav.los_clear(self.pos + up, dest + up) && nav.ground_path_clear(self.pos, dest) {
            self.path.clear();
            self.repath_timer = 0.0;
            // Preferred lateral (evade) velocity, layered onto any radial move already
            // requested this step; the `World`'s integrator commits it. Returns whether
            // the sidestep was clear so `evade_step` can flip direction if walled.
            self.add_move(perp, speed);
            true
        } else {
            false
        }
    }


    /// Gate a step that would carry this hunter through a **shut** door.
    ///
    /// Returns `true` if the hunter should hold this step: it has reached the doorway
    /// and is waiting for the panel to swing. Records the door as a request either way,
    /// so the `World` can start opening it while the hunter is still walking up.
    ///
    /// Both movement branches call this. The A\* branch alone would not be enough — the
    /// straight-line beeline shortcut is gated on `los_clear`, and a door is *not* solid
    /// in the nav grid (it rides the overlay), so a hunter would happily beeline through
    /// a shut door without this.
    fn door_gate(&mut self, nav: &NavWorld, next: Vec3, dt: f32) -> bool {
        let up = Vec3::new(0.0, 0.5, 0.0);
        let Some(di) = nav.door_blocking(self.pos + up, next + up) else {
            self.pending_door = None;
            self.door_wait = 0.0;
            return false;
        };
        self.pending_door = Some(di);
        self.door_wait += dt;
        if self.door_wait > DOOR_WAIT_MAX {
            return false; // safety valve — never stand at a door forever
        }
        let Some(c) = nav.door_center(di) else { return false };
        // Stand clear of the panel's swing, not merely near the doorway: a door opening
        // toward the hunter sweeps out as far as the opening is wide.
        let hold = nav.door_clearance(di).unwrap_or(1.0) + DOOR_WAIT_MARGIN;
        if Vec3::new(c.x - self.pos.x, 0.0, c.z - self.pos.z).length() > hold {
            return false;
        }
        self.moving = false;
        self.move_speed = 0.0;
        true
    }

    fn move_toward(&mut self, dt: f32, target: Vec3, nav: &NavWorld, speed: f32) -> bool {
        // Anti-grind hold: if we were stuck (crowd/separation fight) give it a beat —
        // stand put (legs idle) so the pack settles instead of walk-cycling in place.
        if self.stuck_hold > 0.0 {
            self.stuck_hold -= dt;
            self.moving = false;
            self.move_speed = 0.0;
            return false;
        }
        let flat = Vec3::new(target.x - self.pos.x, 0.0, target.z - self.pos.z);
        if flat.length() < ARRIVE_DIST {
            return true;
        }
        // Sample the walkability line at ~knee height so it clears the floor but
        // catches walls/waist-high obstacles.
        let up = Vec3::new(0.0, 0.5, 0.0);
        // Beeline (straight-line shortcut) ONLY on the same level, clear of walls,
        // and over continuous ground. The height gate is the key one: a switchback
        // landing gives clear line-of-sight up to the next flight (railings are
        // cosmetic and don't block LOS), and the flat beeline would cut a diagonal
        // straight across the open well — through the railing gap — instead of
        // walking the landing to the offset second flight. Forcing every vertical
        // traversal through A* fixes that: A* is 4-connected, so it steps cardinal
        // cell-to-cell along the landing and physically cannot cut the corner.
        let coplanar = (target.y - self.pos.y).abs() <= COPLANAR_BEELINE_EPS;
        if coplanar
            && nav.los_clear(self.pos + up, target + up)
            && nav.ground_path_clear(self.pos, target)
        {
            if self.door_gate(nav, target, dt) {
                return false;
            }
            self.path.clear();
            self.repath_timer = 0.0; // force a fresh A* path the instant LOS breaks
            let dist = flat.length();
            let dir = flat / dist;
            self.heading = dir; // face the (flat) travel direction
            // Preferred velocity toward the target, capped so a single step can't
            // overshoot it (the `World`'s ORCA solve + integrator commit the move —
            // XZ only, with the feet re-snapped to the floor, so the hunter rides
            // gentle rises instead of leaving its Y frozen).
            self.add_move(dir, speed.min(dist / dt.max(1e-6)));
            return false;
        }

        self.repath_timer -= dt;
        if self.repath_timer <= 0.0 {
            self.repath_timer = REPATH_INTERVAL;
            match nav.find_path(self.pos, target) {
                Some(path) => {
                    let last = path.len().saturating_sub(1);
                    self.path = path;
                    self.path_idx = 1.min(last); // skip the start cell
                }
                None => {
                    // Nowhere to walk and no clear line → treat as arrived so the
                    // caller reassigns rather than freezing here forever.
                    self.path.clear();
                    return true;
                }
            }
        }

        if self.path_idx < self.path.len() {
            // Advance past a waypoint already reached (checked on the current,
            // already-integrated position — the integrator commits the move between
            // steps, so `pos` is up to date here).
            if self.pos.distance(self.path[self.path_idx]) < WAYPOINT_EPS
                && self.path_idx < self.path.len() - 1
            {
                self.path_idx += 1;
            }
            let waypoint = self.path[self.path_idx];
            if self.door_gate(nav, waypoint, dt) {
                return false;
            }
            // Aim in the XZ plane toward the waypoint; the tread height is handled by
            // the integrator's floor-snap (A* waypoints are quantized to cell floors,
            // so following them in XZ + snapping never floats the hunter over a step).
            let to = Vec3::new(waypoint.x - self.pos.x, 0.0, waypoint.z - self.pos.z);
            let dist = to.length();
            if dist > 1e-4 {
                let dir = to / dist;
                self.heading = dir;
                self.add_move(dir, speed.min(dist / dt.max(1e-6)));
            }
        }
        false
    }

    /// Choose the spot to juke to for a burst-and-reposition (called on the
    /// Attack→Cooldown transition). Prefers a standable spot that keeps
    /// line-of-sight to the player — so the hunter re-engages from the new angle
    /// rather than juking behind a wall — trying the current weave side first, then
    /// the far side, then shallower arcs; falls back to any standable spot if none
    /// keeps LOS, or `None` if there's nowhere standable to go (→ plain hold-and-face).
    fn pick_reposition(
        &self,
        target_pos: Vec3,
        standoff: f32,
        nav: &NavWorld,
        physics: &mut PhysicsWorld,
        _self_collider: ColliderHandle,
    ) -> Option<Vec3> {
        // Weave around the hunter's own standoff (not a fixed band) so it holds the
        // weapon's engagement range across the reposition.
        let (min_r, max_r) = (standoff * REPOSITION_MIN_FRAC, standoff * REPOSITION_MAX_FRAC);
        let mut fallback = None;
        for (dir, arc) in [
            (self.strafe_dir, REPOSITION_ARC),
            (-self.strafe_dir, REPOSITION_ARC),
            (self.strafe_dir, REPOSITION_ARC * 0.5),
            (-self.strafe_dir, REPOSITION_ARC * 0.5),
        ] {
            let ideal = reposition_point(target_pos, self.pos, dir, arc, min_r, max_r);
            if let Some(spot) = nav.nearest_standable(ideal.x, ideal.y.max(0.1), ideal.z, 3) {
                if fallback.is_none() {
                    fallback = Some(spot);
                }
                if perception_los(physics, spot, target_pos) {
                    return Some(spot);
                }
            }
        }
        fallback
    }

    /// Enter the open burst-and-reposition cooldown (the baseline between-bursts juke):
    /// pick a fresh firing angle to weave to. The caller flips [`Self::strafe_dir`]
    /// first so successive bursts alternate sides.
    fn enter_cooldown_reposition(
        &mut self,
        target_pos: Vec3,
        standoff: f32,
        nav: &NavWorld,
        physics: &mut PhysicsWorld,
        self_collider: ColliderHandle,
    ) {
        self.state = AiState::Cooldown;
        self.cooldown_timer = 0.0;
        self.reposition_target =
            self.pick_reposition(target_pos, standoff, nav, physics, self_collider);
    }

    /// Duck back to a fresh no-LOS cell after a peek volley (#4). If no cover is
    /// reachable any more (the player moved so nothing hides us), give up the cover
    /// loop and fight in the open from `Attack`.
    fn duck_to_cover(
        &mut self,
        target_pos: Vec3,
        nav: &NavWorld,
        physics: &mut PhysicsWorld,
        self_collider: ColliderHandle,
    ) {
        match self.sample_cover_cell(self.pos, target_pos, nav, physics, self_collider, false) {
            Some(spot) => {
                self.cover_target = Some(spot);
                self.peek_target = None;
                self.cover_timer = 0.0;
                self.state = AiState::TakeCover;
            }
            None => {
                self.enter_attack();
            }
        }
    }

    /// Sample standable cells in a ring around `from` and return the nearest one whose
    /// line-of-sight to the player matches `want_los`: `false` finds **cover** (a cell
    /// the player can't see — used to break contact), `true` finds a **peek** cell (one
    /// that sees the player — used to pop out and fire). `None` when no qualifying cell
    /// is found (e.g. an open room has no cover, so the caller falls back to the open
    /// juke). Bounded to [`COVER_SAMPLE_DIRS`]×[`COVER_SAMPLE_RADII`] raycasts, and only
    /// called at a burst-end / peek transition — never every frame.
    fn sample_cover_cell(
        &self,
        from: Vec3,
        target_pos: Vec3,
        nav: &NavWorld,
        physics: &mut PhysicsWorld,
        _self_collider: ColliderHandle,
        want_los: bool,
    ) -> Option<Vec3> {
        let mut best: Option<(f32, Vec3)> = None;
        for k in 0..COVER_SAMPLE_DIRS {
            let ang = k as f32 / COVER_SAMPLE_DIRS as f32 * std::f32::consts::TAU;
            let (s, c) = ang.sin_cos();
            for &r in COVER_SAMPLE_RADII {
                let cand = Vec3::new(from.x + c * r, from.y.max(0.1), from.z + s * r);
                let Some(spot) = nav.nearest_standable(cand.x, cand.y, cand.z, 2) else {
                    continue;
                };
                // The snapped cell must genuinely match the wanted visibility (the
                // snap can pull a candidate back around a corner, so re-check it).
                if perception_los(physics, spot, target_pos) != want_los {
                    continue;
                }
                let d = spot.distance(from);
                if best.map_or(true, |(bd, _)| d < bd) {
                    best = Some((d, spot));
                }
            }
        }
        best.map(|(_, s)| s)
    }
}

/// The ideal burst-and-reposition spot (before snapping to a standable cell): swing
/// `dir · arc` radians around the player from the hunter's current bearing, at the
/// current player-distance clamped to `[min_r, max_r]` (the standoff-relative band).
/// Pure (the nav snap + LOS preference live in [`Enemy::pick_reposition`]).
fn reposition_point(target_pos: Vec3, pos: Vec3, dir: f32, arc: f32, min_r: f32, max_r: f32) -> Vec3 {
    let to = Vec3::new(pos.x - target_pos.x, 0.0, pos.z - target_pos.z);
    let r = to.length();
    if r < 1e-3 {
        return pos; // sitting on the player — no meaningful bearing to arc from
    }
    let ang = to.z.atan2(to.x) + dir * arc;
    let nr = r.clamp(min_r, max_r);
    Vec3::new(
        target_pos.x + ang.cos() * nr,
        pos.y,
        target_pos.z + ang.sin() * nr,
    )
}

/// The flanking approach waypoint: aim for `target` (the player / last-known spot)
/// along a bearing rotated `angle` radians off the direct line, placed
/// [`FLANK_CLOSE_FRAC`] of the current distance *in* — so the hunter curves toward
/// the target from the side while still closing. Recomputing this each step traces a
/// smooth arc into the target. Pure (nav routing to it lives in [`Enemy::move_toward`]).
///
/// `angle == 0` returns a point straight along the current bearing (a plain nearer
/// waypoint → identical to a dead-straight chase), so difficulty 0 is unchanged.
fn flank_point(target: Vec3, pos: Vec3, angle: f32) -> Vec3 {
    let to = Vec3::new(pos.x - target.x, 0.0, pos.z - target.z);
    let r = to.length();
    if r < 1e-3 {
        return target; // sitting on the target — no bearing to offset from
    }
    let ang = to.z.atan2(to.x) + angle;
    let nr = r * FLANK_CLOSE_FRAC;
    Vec3::new(target.x + ang.cos() * nr, pos.y, target.z + ang.sin() * nr)
}

/// **Perception** line-of-sight from `from_feet` to `to_feet`, cast between chest
/// heights but blocked ONLY by world geometry — friendly capsules are ignored (see
/// [`PhysicsWorld::raycast_world_only`]). This is what every FSM engagement / cover
/// decision uses, so a packmate crossing the ray can't make a hunter lose sight of
/// the player (the "gives up for a bit" / engage-flicker bug).
///
/// It is now the **only** sight test. A capsule-blocking variant (`line_of_sight`) used
/// to sit beside it for *shooting*, so a friendly in the line blocked a shot — but the
/// Perfect Dark shot model resolves whoever is on the line instead of being stopped by
/// them (`World::emit_pd_shot`), and `chr_has_los_to_chr` (`chraction.c:6533`) does not
/// put `CDTYPE_CHRS` in its mask either, so nothing wanted it any more.
pub(crate) fn perception_los(physics: &mut PhysicsWorld, from_feet: Vec3, to_feet: Vec3) -> bool {
    let from = from_feet + Vec3::new(0.0, 1.0, 0.0);
    let to = to_feet + Vec3::new(0.0, 0.8, 0.0);
    let d = to - from;
    let dist = d.length();
    if dist < 1e-4 {
        return true;
    }
    let dir = d / dist;
    match physics.raycast_world_only(from, dir, dist) {
        None => true,
        Some(hit) => (hit.point - from).length() >= dist - 0.1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Damage is subtractive off the starting health.
    #[test]
    fn damage_is_subtractive() {
        let mut e = Enemy::new(Vec3::ZERO, Vec3::NEG_Z);
        assert_eq!(e.health(), ENEMY_HEALTH);
        assert!(!e.take_damage(30.0), "not dead after 30 dmg");
        assert_eq!(e.health(), 70.0);
    }

    /// Four PP7 shots (25 dmg) kill the 100-hp hunter; only the lethal shot
    /// returns `true`, and a corpse takes no further damage.
    #[test]
    fn four_25_damage_shots_kill() {
        let mut e = Enemy::new(Vec3::ZERO, Vec3::NEG_Z);
        assert!(!e.take_damage(25.0), "75 hp");
        assert!(!e.take_damage(25.0), "50 hp");
        assert!(!e.take_damage(25.0), "25 hp");
        assert!(e.take_damage(25.0), "lethal shot returns died");
        assert!(e.is_dead());
        assert_eq!(e.health(), 0.0);
        assert!(!e.take_damage(25.0), "a dead hunter takes no more damage");
    }

    /// A hunter facing away from the player does not detect it (cone gate); one
    /// facing toward it (LOS clear, no physics obstacles) alerts.
    #[test]
    fn cone_gates_detection() {
        // Facing +Z, player at −Z (behind) → outside the cone.
        let mut e = Enemy::new(Vec3::ZERO, Vec3::Z);
        let look = Vec3::Z;
        assert!(!e.in_cone(Vec3::new(0.0, 0.0, -5.0), look, DETECTION_HALF_CONE), "player behind is out of cone");
        assert!(e.in_cone(Vec3::new(0.0, 0.0, 5.0), look, DETECTION_HALF_CONE), "player ahead is in cone");
        // Watching toward the player seeds the heading toward it.
        e = Enemy::new(Vec3::ZERO, Vec3::new(0.0, 0.0, 5.0));
        assert!(e.in_cone(Vec3::new(0.0, 0.0, 5.0), Vec3::Z, DETECTION_HALF_CONE));
    }

    /// Proximity sense: a player standing right next to the hunter is noticed even
    /// when behind it (out of cone), but only with line-of-sight, and not once beyond
    /// proximity range while still behind.
    #[test]
    fn close_player_noticed_even_from_behind() {
        let e = Enemy::new(Vec3::ZERO, Vec3::Z); // facing +Z
        let look = Vec3::Z;
        let behind_close = Vec3::new(0.0, 0.0, -2.0); // behind, within PROXIMITY_RANGE
        assert!(!e.in_cone(behind_close, look, DETECTION_HALF_CONE), "the close player is behind the cone");
        assert!(e.perceives(behind_close, true, 1.0, look, DETECTION_HALF_CONE), "…but proximity notices it");
        assert!(!e.perceives(behind_close, false, 1.0, look, DETECTION_HALF_CONE), "…unless a wall blocks LOS");
        let behind_far = Vec3::new(0.0, 0.0, -8.0); // behind AND beyond proximity
        assert!(!e.perceives(behind_far, true, 1.0, look, DETECTION_HALF_CONE), "far + behind stays unnoticed");
    }

    /// A freshly-spawned hunter starts hunting (Search), not standing idle — it
    /// flooded in through the door without knowing where the player is.
    #[test]
    fn new_hunter_starts_searching() {
        let e = Enemy::new(Vec3::ZERO, Vec3::NEG_Z);
        assert_eq!(e.state(), AiState::Search);
        assert!(e.search_target().is_none(), "no point assigned yet");
    }

    /// Assigning a search point stores it (and the `World` reads it back to fan the
    /// pack out); a dead hunter ignores the assignment.
    #[test]
    fn assign_search_target_stores_and_reads_back() {
        let mut e = Enemy::new(Vec3::ZERO, Vec3::NEG_Z);
        let t = Vec3::new(4.0, 0.0, 2.0);
        e.assign_search_target(t);
        assert_eq!(e.search_target(), Some(t));
        e.take_damage(ENEMY_HEALTH); // kill
        e.assign_search_target(Vec3::new(9.0, 0.0, 9.0));
        assert_eq!(e.search_target(), Some(t), "a corpse ignores new orders");
    }

    /// The burst-and-reposition point swings the hunter around the player by the arc
    /// (sign = weave direction), lands at the player-distance clamped into the band,
    /// and flips sides with the direction. A hunter sitting on the player keeps its
    /// spot (no bearing to arc from).
    #[test]
    fn reposition_point_arcs_around_the_player() {
        let player = Vec3::ZERO;
        // Hunter 4 m due +X of the player (bearing 0). Arc +0.7 rad should rotate the
        // bearing that far while staying at ~4 m (inside the [3, 5.5] band).
        // A standoff-relative band around a 3 m standoff: [2.4, 4.2].
        let (min_r, max_r) = (2.4, 4.2);
        let pos = Vec3::new(4.0, 0.0, 0.0);
        let p = reposition_point(player, pos, 1.0, REPOSITION_ARC, min_r, max_r);
        let r = (p.x * p.x + p.z * p.z).sqrt();
        assert!((r - 4.0).abs() < 1e-3, "stays at the clamped radius, got {r}");
        let ang = p.z.atan2(p.x);
        assert!((ang - REPOSITION_ARC).abs() < 1e-3, "arced +{REPOSITION_ARC} rad, got {ang}");
        // Flipping the direction arcs to the mirror bearing.
        let q = reposition_point(player, pos, -1.0, REPOSITION_ARC, min_r, max_r);
        assert!((q.z.atan2(q.x) + REPOSITION_ARC).abs() < 1e-3, "the other side mirrors");
        // Too close → the radius is pushed out to the min band, not left inside it.
        let close = Vec3::new(1.0, 0.0, 0.0);
        let c = reposition_point(player, close, 1.0, REPOSITION_ARC, min_r, max_r);
        let cr = (c.x * c.x + c.z * c.z).sqrt();
        assert!((cr - min_r).abs() < 1e-3, "clamped up to the min band, got {cr}");
        // Sitting on the player → unchanged (no bearing).
        assert_eq!(reposition_point(player, player, 1.0, REPOSITION_ARC, min_r, max_r), player);
    }

    /// A gunshot pulls a *searching* hunter to investigate the sound (last-known set
    /// to the noise, state → Investigate), but a hunter already *engaged* keeps its
    /// own better information.
    #[test]
    fn hear_noise_diverts_only_a_seeker() {
        let noise = Vec3::new(3.0, 0.0, 5.0);

        let mut seeker = Enemy::new(Vec3::ZERO, Vec3::NEG_Z); // starts in Search
        seeker.hear_noise(noise);
        assert_eq!(seeker.state(), AiState::Investigate);
        assert_eq!(seeker.last_known, Some(noise));

        let mut engaged = Enemy::new(Vec3::ZERO, Vec3::NEG_Z);
        engaged.state = AiState::Attack; // mid-fight — has eyes on the player
        engaged.hear_noise(noise);
        assert_eq!(engaged.state(), AiState::Attack, "an engaged hunter isn't distracted");
    }

    /// #6 sharper senses: a player straight ahead (in cone) just past the baseline
    /// 12 m detection range is unseen at baseline `sense` (1.0), but a difficulty-
    /// sharpened hunter (`sense` 1.4) reaches them. Locks the perception-range lever.
    #[test]
    fn sharper_senses_extend_the_detection_range() {
        let e = Enemy::new(Vec3::ZERO, Vec3::Z); // facing +Z
        let ahead_far = Vec3::new(0.0, 0.0, 14.0); // in cone, beyond DETECTION_RANGE (12)
        let look = Vec3::Z;
        assert!(e.in_cone(ahead_far, look, DETECTION_HALF_CONE), "target is dead ahead");
        assert!(!e.perceives(ahead_far, true, 1.0, look, DETECTION_HALF_CONE), "beyond baseline sight range");
        assert!(e.perceives(ahead_far, true, 1.4, look, DETECTION_HALF_CONE), "sharper senses reach it (14 < 12·1.4)");
    }

    /// An open baked room + an empty physics world (so line-of-sight is always clear),
    /// for driving the FSM headlessly.
    fn open_room() -> NavWorld {
        use engine::geometry::csg_runtime::{Brush, Op, Region};
        let mut regions = {
            let mut r = Region::new(0);
            // 80×80 WT = 20 m square, plenty of room for a 9 m approach.
            r.brushes.push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 80.0, 16.0, 80.0));
            vec![r]
        };
        engine::sim::nav::bake(&mut regions, &[]).expect("room bakes")
    }

    /// Drive one FSM step then commit the movement the way the `World` does for a lone
    /// hunter: with no packmates, ORCA is a no-op, so the preferred velocity is applied
    /// straight through the integrator. Mirrors the two-stage update→integrate pipeline
    /// (`Enemy::update` now only *decides* a velocity; the `World` commits it) so the
    /// movement-dependent FSM tests still exercise real motion.
    #[allow(clippy::too_many_arguments)]
    fn drive(
        e: &mut Enemy,
        dt: f32,
        player: Vec3,
        standoff: f32,
        tuning: AiTuning,
        aimed: bool,
        nav: &NavWorld,
        physics: &mut PhysicsWorld,
        fire_anim: bool,
        collider: ColliderHandle,
    ) -> EnemyStep {
        let step = e.update(
            dt,
            EngageTarget::player(player),
            player,
            standoff,
            DistBand::DEFAULT,
            tuning,
            aimed,
            nav,
            physics,
            fire_anim,
            collider,
        );
        let dv = e.desired_vel;
        e.integrate_move(dv, dt, nav);
        step
    }

    /// Drive one step against an arbitrary [`EngageTarget`], with the player somewhere
    /// else — the free-for-all path.
    #[allow(clippy::too_many_arguments)]
    fn drive_target(
        e: &mut Enemy,
        dt: f32,
        target: EngageTarget,
        player: Vec3,
        standoff: f32,
        tuning: AiTuning,
        nav: &NavWorld,
        physics: &mut PhysicsWorld,
        collider: ColliderHandle,
    ) -> EnemyStep {
        let step = e.update(
            dt, target, player, standoff, DistBand::DEFAULT, tuning, false, nav, physics, false,
            collider,
        );
        let dv = e.desired_vel;
        e.integrate_move(dv, dt, nav);
        step
    }

    /// **A target one floor up is not "at standoff".**
    ///
    /// The engagement band used to be measured laterally, so a hunter on the storey
    /// below its target read a comfortable horizontal distance, decided it was exactly
    /// where it wanted to be, and planted — with a floor slab between it and the shot.
    /// Perfect Dark measures this band with the 3D `chr_get_distance_to_coord`
    /// (`botcmd.c:98`) and keeps a separate lateral function for arrival tests, which is
    /// the split this mirrors.
    ///
    /// The numbers are the ones from the playtest that found it: a hunter 5.3 m away
    /// horizontally and one storey (3 m) down, holding a pistol standoff of 4.8 m.
    #[test]
    fn a_target_a_floor_above_is_out_of_the_standoff_band() {
        let e = Enemy::new(Vec3::new(0.0, 0.0, 0.0), Vec3::Z);
        let above = Vec3::new(5.3, 3.0, 0.0);
        let standoff = 4.8; // `standoff_for(8.0)` — the PP7
        let band = standoff - STANDOFF_HYST..=standoff + STANDOFF_HYST;

        let lateral = e.dist_to(above);
        assert!((lateral - 5.3).abs() < 1e-3, "the lateral reach is unchanged");
        assert!(band.contains(&lateral), "…and it looks like a perfect firing distance");

        let real = e.engage_dist(above);
        assert!((real - 6.09).abs() < 0.02, "the real separation is 6.1 m, got {real:.2}");
        assert!(!band.contains(&real), "which is outside the band, so the hunter closes");

        // Same floor, and the two agree — the fix cannot have moved the flat case.
        let level = Vec3::new(5.3, 0.0, 0.0);
        assert!((e.engage_dist(level) - e.dist_to(level)).abs() < 1e-4);
    }

    /// **A hunter that cannot see its target closes, whatever the distance says.**
    ///
    /// `botcmd_tick_dist_mode` says this twice (`botcmd.c:135` and `:163`): an
    /// out-of-sight target turns both `OK` (hold at standoff) and `BACKUP` (give ground)
    /// into `ADVANCE`. We had no such rule — only `ATTACK_LOS_GRACE`, a *debounce* on the
    /// Attack→Chase bail. Where the sightline flickers rather than cleanly breaking —
    /// heads bobbing over a floor edge, a corner seam — the grace keeps resetting, so the
    /// hunter never bails and never advances: it stands inside its standoff band
    /// indefinitely, holding a distance it has no shot along. Which is exactly what the
    /// playtest screenshot showed.
    ///
    /// `detectable = false` is the sightline block here: it is the one input that makes
    /// `can_see` false without needing level geometry in the test.
    #[test]
    fn an_attacking_hunter_with_no_sightline_advances_instead_of_holding() {
        let nav = open_room();
        let dt = 1.0 / 60.0;
        let standoff = 4.8;
        let player = Vec3::new(10.0, 0.05, 15.0);
        // Planted at exactly its standoff — the state the stall was observed in.
        let start = Vec3::new(10.0, 0.05, 15.0 - standoff);

        let run = |can_see: bool| -> (bool, f32, f32) {
            let mut physics = PhysicsWorld::new(); // empty → geometry never blocks
            let collider = physics.add_enemy_collider(start, 0.24, 0.48);
            let mut e = Enemy::new(start, player);
            e.state = AiState::Attack;
            e.holding = true;
            e.set_detectable(can_see);
            // One step, so the Attack arm runs before the LOS grace can bail it to Chase.
            drive(&mut e, dt, player, standoff, AiTuning::default(), false, &nav, &mut physics, false, collider);
            (e.holding, e.desired_vel.length(), e.dist_to(player))
        };

        let (holding, speed, _) = run(false);
        assert!(!holding, "a hunter with no sightline must not hold its standoff");
        assert!(speed > 0.1, "it advances; desired speed was {speed:.3} m/s");

        // The control: with a clear sightline at the same distance it plants, which is
        // the behaviour the standoff exists for and must not have been broken.
        let (holding, speed, _) = run(true);
        assert!(holding, "with a clear sightline it holds at standoff");
        assert!(speed < 0.1, "and stands still; desired speed was {speed:.3} m/s");
    }

    /// **A hunter engaging a packmate manoeuvres against the packmate, not the player.**
    ///
    /// This is the free-for-all seam. Before it, `update` took the player's position and
    /// nothing else, so a Perfect Dark simulant that picked a packmate would shoot it
    /// (the round resolves against whoever is on the line) while still closing on,
    /// standing off from and taking cover against the *player* — it fired from wherever
    /// it happened to be standing.
    ///
    /// The hunter starts between the two, with the packmate 9 m along `+X` and the
    /// player 9 m along `−X`, so "closed on its target" and "closed on the player" are
    /// opposite signs and no amount of tuning can make one look like the other.
    #[test]
    fn a_hunter_closes_on_the_packmate_it_is_engaging_not_on_the_player() {
        let nav = open_room();
        let dt = 1.0 / 60.0;
        let start = Vec3::new(10.0, 0.05, 10.0);
        let packmate = Vec3::new(19.0, 0.05, 10.0);
        let player = Vec3::new(1.0, 0.05, 10.0);
        let tuning = AiTuning::default();

        let mut physics = PhysicsWorld::new(); // empty → LOS always clear
        let collider = physics.add_enemy_collider(start, 0.24, 0.48);
        let mut e = Enemy::new(start, packmate); // watching toward the packmate
        for _ in 0..400 {
            drive_target(
                &mut e,
                dt,
                EngageTarget::hunter(1, packmate),
                player,
                3.0,
                tuning,
                &nav,
                &mut physics,
                collider,
            );
        }
        let to_packmate = e.dist_to(packmate);
        let to_player = e.dist_to(player);
        assert!(
            to_packmate < 9.0 - 2.0,
            "it should have closed on the packmate; {to_packmate:.2} m away (started 9)"
        );
        assert!(
            to_player > 9.0,
            "and moved AWAY from the player; {to_player:.2} m away (started 9)"
        );
        // It engaged: the packmate became its knowledge, and it wants to shoot it.
        assert_eq!(
            e.last_known.map(|p| p.x > start.x),
            Some(true),
            "the last-known position is the packmate's side of the room",
        );

        // The control: the same hunter handed the PLAYER instead goes the other way.
        let mut physics = PhysicsWorld::new();
        let collider = physics.add_enemy_collider(start, 0.24, 0.48);
        let mut e = Enemy::new(start, player);
        for _ in 0..400 {
            drive(&mut e, dt, player, 3.0, tuning, false, &nav, &mut physics, false, collider);
        }
        assert!(
            e.dist_to(player) < 9.0 - 2.0 && e.dist_to(packmate) > 9.0,
            "the player-targeted control closes the other way ({:.2} m from the player)",
            e.dist_to(player),
        );
    }

    /// The catch check stays **player-only**: a hunter standing on top of a packmate it
    /// is engaging has not caught anybody, or a free-for-all would end the hunt the
    /// moment two simulants collided.
    #[test]
    fn catching_a_packmate_does_not_end_the_hunt() {
        let nav = open_room();
        let mut physics = PhysicsWorld::new();
        let here = Vec3::new(10.0, 0.05, 10.0);
        let collider = physics.add_enemy_collider(here, 0.24, 0.48);
        let mut e = Enemy::new(here, here);
        let far_player = Vec3::new(10.0, 0.05, 25.0);
        let step = drive_target(
            &mut e,
            1.0 / 60.0,
            EngageTarget::hunter(1, here), // right on top of it
            far_player,
            3.0,
            AiTuning::default(),
            &nav,
            &mut physics,
            collider,
        );
        assert!(!step.caught, "a packmate at zero range is not a caught player");
        // …and the player at zero range still is.
        let step = drive_target(
            &mut e,
            1.0 / 60.0,
            EngageTarget::hunter(1, here),
            here,
            3.0,
            AiTuning::default(),
            &nav,
            &mut physics,
            collider,
        );
        assert!(step.caught, "the player at zero range is");
    }

    /// The `N` invisibility toggle is a **player** aid: it must not blind a hunter to a
    /// packmate it is engaging, or turning it on would freeze every simulant fight.
    #[test]
    fn the_invisibility_toggle_does_not_hide_packmates() {
        let mut physics = PhysicsWorld::new(); // empty → geometry never blocks
        let here = Vec3::new(10.0, 0.05, 10.0);
        let mut e = Enemy::new(here, here);
        e.set_detectable(false);
        let packmate = Vec3::new(14.0, 0.05, 10.0);
        assert!(
            e.can_see(EngageTarget::hunter(1, packmate), &mut physics),
            "a packmate is still visible with the player hidden"
        );
        assert!(
            !e.can_see(EngageTarget::player(packmate), &mut physics),
            "…while the player at the same spot is not"
        );
    }

    /// Player-visibility toggle (the `N` dev/observe aid): with `detectable` false, a
    /// hunter never perceives a player in plain sight — no last-known, no fire — so it
    /// stays blind (searching); with `detectable` true the same setup acquires it.
    #[test]
    fn undetectable_player_is_never_perceived() {
        let nav = open_room();
        let dt = 1.0 / 60.0;
        let player = Vec3::new(4.0, 0.0, 10.0);
        // Returns (acquired the player?, ever wanted to fire?) after a run.
        let run = |detectable: bool| -> (bool, bool) {
            let mut physics = PhysicsWorld::new(); // empty → LOS always clear
            let feet = Vec3::new(4.0, 0.05, 4.0); // ~6 m away, facing the player
            let collider = physics.add_enemy_collider(feet, 0.24, 0.48);
            let mut e = Enemy::new(feet, player);
            let tuning = AiTuning { sense: 1.4, ..AiTuning::default() };
            let mut ever_fire = false;
            for _ in 0..300 {
                e.set_detectable(detectable); // the `World` sets this each step
                let step = drive(&mut e, dt, player, 3.0, tuning, false, &nav, &mut physics, false, collider);
                ever_fire |= step.want_fire;
            }
            (e.last_known().is_some(), ever_fire)
        };

        let (seen, _) = run(true);
        assert!(seen, "a detectable player in plain sight should be perceived");
        let (acquired, fired) = run(false);
        assert!(!acquired, "an invisible player must never be perceived (no last-known)");
        assert!(!fired, "an invisible player must never draw fire");
    }

    /// #2 suppressing fire: a difficulty-aggressive hunter (`suppress` 1) opens fire
    /// while it's still closing — beyond its weapon's attack range — whereas a baseline
    /// hunter (`suppress` 0) holds fire until it has closed to that range. Drives the
    /// real FSM from spawn through Alert into Chase in a headless room.
    #[test]
    fn suppressing_fire_opens_up_while_still_closing() {
        let nav = open_room();
        let standoff = 3.0; // shotgun-like → attack_range = standoff + ATTACK_FIRE_BAND = 6
        let attack_range = standoff + ATTACK_FIRE_BAND;

        // The horizontal distance at which the hunter first *requests* a fire burst,
        // driven from spawn (facing the player, ~9 m off) through Alert into Chase.
        let first_fire_dist = |suppress: f32| -> Option<f32> {
            let mut physics = PhysicsWorld::new();
            let player = Vec3::new(4.0, 0.0, 13.0);
            let feet = Vec3::new(4.0, 0.05, 4.0); // ~9 m from the player, facing them
            let collider = physics.add_enemy_collider(feet, 0.24, 0.48);
            let mut e = Enemy::new(feet, player);
            // Short reaction so it reaches Chase quickly; sharp senses so 9 m is well
            // within perception either way (isolating `suppress` as the only variable).
            let tuning = AiTuning { alert: 0.05, suppress, sense: 1.4, ..AiTuning::default() };
            let dt = 1.0 / 60.0;
            for _ in 0..600 {
                let step = drive(&mut e, dt, player, standoff, tuning, false, &nav, &mut physics, false, collider);
                let d = Vec3::new(e.pos.x - player.x, 0.0, e.pos.z - player.z).length();
                if step.want_fire {
                    return Some(d);
                }
                if d < 1.0 {
                    break; // reached the player without ever firing
                }
            }
            None
        };

        let hot = first_fire_dist(1.0).expect("a suppressing hunter fires");
        assert!(
            hot > attack_range,
            "suppressing fire starts before reaching attack range (fired at {hot:.1} m, range {attack_range})"
        );

        let cold = first_fire_dist(0.0).expect("a baseline hunter eventually fires");
        assert!(
            cold <= attack_range + 0.5,
            "baseline holds fire until it has closed to standoff (fired at {cold:.1} m)"
        );
    }

    /// #3 flank point (pure): at angle 0 it's a nearer waypoint straight on the direct
    /// line (a plain closing move — the difficulty-0 baseline); a nonzero angle swings
    /// the aim point to one side (sign = which side), keeping the closed-in radius.
    #[test]
    fn flank_point_offsets_the_approach_bearing() {
        let target = Vec3::ZERO;
        let pos = Vec3::new(4.0, 0.0, 0.0); // hunter 4 m due +X of the target (bearing 0)

        let straight = flank_point(target, pos, 0.0);
        assert!(straight.z.abs() < 1e-4, "no lateral offset at angle 0");
        assert!(straight.x > 0.0 && straight.x < pos.x, "a nearer waypoint on the line");
        assert!(
            (straight.x - 4.0 * FLANK_CLOSE_FRAC).abs() < 1e-4,
            "aims FLANK_CLOSE_FRAC of the way in"
        );

        let left = flank_point(target, pos, 0.6);
        let right = flank_point(target, pos, -0.6);
        assert!(left.z > 0.0, "a positive angle offsets to one side");
        assert!(right.z < 0.0, "a negative angle offsets to the other");
        let rl = (left.x * left.x + left.z * left.z).sqrt();
        assert!((rl - 4.0 * FLANK_CLOSE_FRAC).abs() < 1e-4, "still closes in (kept the radius)");

        // Sitting on the target → unchanged (no bearing to offset from).
        assert_eq!(flank_point(target, target, 0.6), target);
    }

    /// #3 flanking (FSM): a fully-flanking chaser (`flank` 1) curves well off the direct
    /// start→player line while closing, whereas a baseline chaser (`flank` 0) runs
    /// straight down it. Drives the real FSM from spawn through Alert into Chase.
    #[test]
    fn a_flanking_chaser_curves_off_the_direct_line() {
        let nav = open_room();
        let standoff = 3.0;
        let attack_range = standoff + ATTACK_FIRE_BAND;

        // Max lateral deviation (|x − start_x|) over the approach; the player sits
        // straight +Z of the spawn, so a straight chase keeps x fixed.
        let max_lateral = |flank: f32| -> f32 {
            let mut physics = PhysicsWorld::new();
            let player = Vec3::new(6.0, 0.0, 16.0);
            let feet = Vec3::new(6.0, 0.05, 6.0); // 10 m due −Z of the player, same X
            let collider = physics.add_enemy_collider(feet, 0.24, 0.48);
            let mut e = Enemy::new(feet, player);
            e.set_flank_side(1.0);
            let tuning = AiTuning { alert: 0.05, flank, sense: 1.4, ..AiTuning::default() };
            let dt = 1.0 / 60.0;
            let mut max_dx = 0.0f32;
            for _ in 0..600 {
                drive(&mut e, dt, player, standoff, tuning, false, &nav, &mut physics, false, collider);
                max_dx = max_dx.max((e.pos.x - 6.0).abs());
                let d = Vec3::new(e.pos.x - player.x, 0.0, e.pos.z - player.z).length();
                if d <= attack_range {
                    break; // reached attack range — done approaching
                }
            }
            max_dx
        };

        let straight = max_lateral(0.0);
        let flanked = max_lateral(1.0);
        assert!(straight < 0.5, "a baseline chaser stays on the direct line (dx {straight:.2})");
        assert!(flanked > 1.0, "a flanking chaser curves well off it (dx {flanked:.2})");
        assert!(flanked > straight + 0.5, "flanking deviates more than a straight chase");
    }

    /// A short fire-burst driver for headless FSM tests: mirrors the world combat loop
    /// (a `want_fire` request starts a ~0.5 s burst during which `fire_anim` is true),
    /// so the Attack/Peek burst-end transitions fire.
    struct FireSim {
        burst: i32,
    }
    impl FireSim {
        fn new() -> Self {
            Self { burst: 0 }
        }
        /// The `fire_anim` flag to feed this frame.
        fn anim(&self) -> bool {
            self.burst > 0
        }
        /// Fold in this frame's `want_fire`, then age the burst.
        fn tick(&mut self, want_fire: bool) {
            if want_fire {
                self.burst = 30; // ~0.5 s at 60 fps
            } else if self.burst > 0 {
                self.burst -= 1;
            }
        }
    }

    /// #1/#4 sampler: with a physics occluder in the room, `sample_cover_cell` finds a
    /// standable cell with NO line-of-sight to the player (cover) when asked for one,
    /// and a cell WITH line-of-sight (a peek spot) when asked for that. (The occluder
    /// is physics-only — nav stays open; enemies don't collide with it, so this isolates
    /// the LOS logic — see [[enemy-nav-vs-physics]].)
    #[test]
    fn sample_finds_cover_and_peek_cells() {
        let nav = open_room();
        let mut physics = PhysicsWorld::new();
        // A 2 m-square, 3 m-tall pillar at (10, 10) — a static LOS occluder.
        physics.add_door_collider(Vec3::new(9.0, 0.0, 9.0), Vec3::new(11.0, 3.0, 11.0));
        let player = Vec3::new(10.0, 0.0, 4.0);
        let behind = Vec3::new(10.0, 0.05, 13.0); // straight behind the pillar
        let collider = physics.add_enemy_collider(behind, 0.24, 0.48);
        let e = Enemy::new(behind, player);

        // Behind the pillar → a no-LOS cover cell is found (and is genuinely hidden).
        let cover = e
            .sample_cover_cell(behind, player, &nav, &mut physics, collider, false)
            .expect("a cell behind the pillar breaks LOS");
        assert!(
            !perception_los(&mut physics, cover, player),
            "the cover cell truly has no LOS to the player"
        );

        // In the open in front of the pillar → a peek (LOS) cell is found.
        let open = Vec3::new(10.0, 0.05, 6.5);
        let peek = e
            .sample_cover_cell(open, player, &nav, &mut physics, collider, true)
            .expect("a cell in the open sees the player");
        assert!(
            perception_los(&mut physics, peek, player),
            "the peek cell truly has LOS to the player"
        );
    }

    /// #1/#4 (FSM): with cover available, an engaged hunter breaks off to a no-LOS
    /// cell between bursts, actually hides there, then pops back out to peek-fire.
    #[test]
    fn a_hunter_breaks_to_cover_and_peeks() {
        let nav = open_room();
        let mut physics = PhysicsWorld::new();
        // A wall at z∈[6,7] spanning the room; the player sits at z=3, so cells at
        // z>7 are hidden from them (physics LOS occluder only; nav stays open).
        physics.add_door_collider(Vec3::new(0.0, 0.0, 6.0), Vec3::new(20.0, 3.0, 7.0));
        let player = Vec3::new(10.0, 0.0, 3.0);
        let feet = Vec3::new(10.0, 0.05, 5.0); // near the wall, in the player's sight
        let collider = physics.add_enemy_collider(feet, 0.24, 0.48);
        let mut e = Enemy::new(feet, player);
        let tuning = AiTuning { alert: 0.05, cover: 1.0, sense: 1.4, ..AiTuning::default() };
        let standoff = 3.0;
        let dt = 1.0 / 60.0;

        let mut fire = FireSim::new();
        let (mut took_cover, mut hidden, mut peeked) = (false, false, false);
        for _ in 0..1200 {
            // 20 s
            let step =
                drive(&mut e, dt, player, standoff, tuning, false, &nav, &mut physics, fire.anim(), collider);
            fire.tick(step.want_fire);
            match e.state() {
                AiState::TakeCover => {
                    took_cover = true;
                    if !perception_los(&mut physics, e.pos, player) {
                        hidden = true;
                    }
                }
                AiState::Peek => peeked = true,
                _ => {}
            }
            if took_cover && hidden && peeked {
                break;
            }
        }
        assert!(took_cover, "the hunter broke off to cover");
        assert!(hidden, "it reached a spot with no LOS to the player");
        assert!(peeked, "it popped back out to peek-fire");
    }

    /// #1/#4 graceful degradation: in an open room (no occluder → no cover cell exists),
    /// a cover-capable hunter falls back to the open burst-and-reposition (Cooldown) and
    /// never enters the cover states.
    #[test]
    fn no_cover_falls_back_to_the_open_reposition() {
        let nav = open_room();
        let mut physics = PhysicsWorld::new(); // empty → LOS is always clear (no cover)
        let player = Vec3::new(10.0, 0.0, 10.0);
        let feet = Vec3::new(10.0, 0.05, 6.0);
        let collider = physics.add_enemy_collider(feet, 0.24, 0.48);
        let mut e = Enemy::new(feet, player);
        let tuning = AiTuning { alert: 0.05, cover: 1.0, sense: 1.4, ..AiTuning::default() };
        let standoff = 3.0;
        let dt = 1.0 / 60.0;

        let mut fire = FireSim::new();
        let (mut cooled, mut took_cover) = (false, false);
        for _ in 0..1200 {
            let step =
                drive(&mut e, dt, player, standoff, tuning, false, &nav, &mut physics, fire.anim(), collider);
            fire.tick(step.want_fire);
            match e.state() {
                AiState::Cooldown => cooled = true,
                AiState::TakeCover | AiState::Peek => took_cover = true,
                _ => {}
            }
            if cooled {
                break;
            }
        }
        assert!(cooled, "with no cover it uses the open burst-and-reposition");
        assert!(!took_cover, "it never enters the cover states when there's nowhere to hide");
    }

    /// A hunter walking into a **shut** door pulls up at the threshold and asks for it
    /// to be opened, rather than ghosting through it.
    ///
    /// This is checked on the *beeline*, which is the case that would otherwise slip
    /// through: the straight-line shortcut is gated on `los_clear`, and a door is not a
    /// solid cell (it rides the nav overlay), so without the gate a hunter would walk
    /// straight through a closed door in plain sight.
    #[test]
    fn a_hunter_holds_at_a_shut_door_and_asks_for_it() {
        use engine::geometry::csg_runtime::{Brush, Op, Region};
        use engine::sim::nav;

        // One room (a subtract carves the cavity) with a door slab across the middle.
        let mut regions = {
            let mut r = Region::new(0);
            r.brushes.push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 24.0, 16.0, 24.0));
            vec![r]
        };
        let mut navw = nav::bake(&mut regions, &[]).expect("bake");
        let mut door = Brush::new(1, Op::Subtract, 12.0, 0.0, 0.0, 1.0, 7.0, 24.0);
        door.door = true;
        navw.set_doors(&[door]);

        // Start a couple of metres short of the door, heading across it.
        let start = Vec3::new(10.0 * WT, 0.1, 12.0 * WT);
        let target = Vec3::new(16.0 * WT, 0.1, 12.0 * WT);
        let mut e = Enemy::new(start, Vec3::X);

        // Walk until it either reaches the door or gives up.
        let dt = 1.0 / 120.0;
        let mut held = false;
        for _ in 0..600 {
            e.desired_vel = Vec3::ZERO;
            e.move_toward(dt, target, &navw, 2.0);
            if e.pending_door.is_some() && !e.moving {
                held = true;
                break;
            }
            let v = e.desired_vel;
            e.integrate_move(v, dt, &navw);
        }
        assert!(held, "the hunter stopped at the shut door and requested it");
        assert_eq!(e.pending_door, Some(0), "and named the door in its way");
        assert!(
            e.pos.x < 12.0 * WT,
            "it never crossed the panel: stopped at x={}",
            e.pos.x / WT
        );

        // Open the door — the hold clears and it walks on through.
        navw.set_door_open(0, true);
        for _ in 0..600 {
            e.desired_vel = Vec3::ZERO;
            e.move_toward(dt, target, &navw, 2.0);
            let v = e.desired_vel;
            e.integrate_move(v, dt, &navw);
        }
        assert_eq!(e.pending_door, None, "nothing blocking once it is open");
        assert!(e.pos.x > 12.0 * WT, "and the hunter is through the doorway");
    }

    /// The door request must survive the **real** `update` path, whichever AI branch is
    /// live. `update` returns from three places (Perfect Dark's ladder, the default-on
    /// utility scorer, the legacy FSM), and the first version of this feature reported
    /// the door through a field on `EnemyStep` that only the last of those set — so in
    /// the actual game every request was dropped and hunters just clipped through doors
    /// once the wait valve expired. Reading it off the hunter can't rot that way.
    #[test]
    fn a_door_request_survives_every_ai_branch() {
        use engine::geometry::csg_runtime::{Brush, Op, Region};
        use engine::sim::nav;

        // Both of the *pathing* branches. Perfect Dark's ladder is deliberately not
        // driven here: a PD hunter that can see its target stops at its distance band
        // and shoots rather than walking anywhere, so the scenario would be testing the
        // AI's mood rather than this wiring. It shares the same `move_toward`, and
        // `pending_door` now lives on the hunter rather than on whichever branch's
        // return value, which is what makes the branch irrelevant.
        let mode = AiMode::Ours;
        {
            for utility in [true, false] {
                let mut regions = {
                    let mut r = Region::new(0);
                    r.brushes
                        .push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 24.0, 16.0, 24.0));
                    vec![r]
                };
                let nav = {
                    let mut n = nav::bake(&mut regions, &[]).expect("bake");
                    let mut door = Brush::new(1, Op::Subtract, 12.0, 0.0, 0.0, 1.0, 7.0, 24.0);
                    door.door = true;
                    n.set_doors(&[door]);
                    n
                };
                let mut physics = PhysicsWorld::new();
                let start = Vec3::new(9.0 * WT, 0.1, 12.0 * WT);
                let player = Vec3::new(18.0 * WT, 0.1, 12.0 * WT);
                let collider = physics.add_enemy_collider(start, 0.24, 0.48);
                let mut e = Enemy::new(start, Vec3::X);
                e.utility = utility;
                e.omniscient = true; // head straight for the player, through the door
                let tuning = AiTuning { mode, ..AiTuning::default() };

                let dt = 1.0 / 120.0;
                let mut saw_request = false;
                for _ in 0..900 {
                    drive(&mut e, dt, player, 1.0, tuning, false, &nav, &mut physics, false, collider);
                    if e.pending_door().is_some() {
                        saw_request = true;
                        break;
                    }
                }
                assert!(
                    saw_request,
                    "mode={mode:?} utility={utility}: the hunter reported the shut door"
                );
            }
        }
    }

    /// A hunter waits far enough back to clear the panel's swing — a door opening
    /// *toward* it sweeps out as wide as the opening, and nothing physical stops it
    /// (hunters navigate the grid and ignore the panel collider).
    #[test]
    fn a_hunter_stands_clear_of_a_doors_swing() {
        use engine::geometry::csg_runtime::{Brush, Op, Region};
        use engine::sim::nav;

        // A wide (6 WT = 1.5 m) double opening: its leaves sweep correspondingly far.
        let mut regions = {
            let mut r = Region::new(0);
            r.brushes.push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 24.0, 16.0, 24.0));
            vec![r]
        };
        let mut navw = nav::bake(&mut regions, &[]).expect("bake");
        let mut door = Brush::new(1, Op::Subtract, 12.0, 0.0, 9.0, 1.0, 7.0, 6.0);
        door.door = true;
        navw.set_doors(&[door]);
        let clearance = navw.door_clearance(0).expect("clearance");
        assert!((clearance - 6.0 * WT).abs() < 1e-4, "clearance is the opening span");

        // Start well outside the hold radius so the walk-in is actually exercised —
        // beginning inside it would "pass" without the hunter ever stopping itself.
        let start = Vec3::new(2.0 * WT, 0.1, 12.0 * WT);
        let target = Vec3::new(18.0 * WT, 0.1, 12.0 * WT);
        let mut e = Enemy::new(start, Vec3::X);
        assert!(
            (12.5 * WT - start.x).abs() > clearance,
            "precondition: the hunter starts clear of the swing and must walk in"
        );
        let dt = 1.0 / 120.0;
        for _ in 0..900 {
            e.desired_vel = Vec3::ZERO;
            e.move_toward(dt, target, &navw, 2.0);
            if e.pending_door.is_some() && !e.moving {
                break;
            }
            let v = e.desired_vel;
            e.integrate_move(v, dt, &navw);
        }
        let gap = (12.5 * WT - e.pos.x).abs();
        assert!(
            gap >= clearance,
            "held {gap:.2} m from the doorway, needs at least the {clearance:.2} m swing"
        );
    }

    /// **The scoring regression, isolated.** A hunter holding a fresh `last_known` — one
    /// heard gunshot is enough — used to have `Search` scored at zero (it requires
    /// `last_known.is_none()`) and so fell to `Investigate`, which walks to the player's
    /// last-known position. That was the beeline: an empty-handed hunter closing on the
    /// player because the only behaviour available to it happened to point that way.
    ///
    /// The fetch point sits 9 m one way and the belief 9 m the other, so "went shopping"
    /// and "beelined" are opposite signs. Run in **all three** decision layers, because
    /// the last version of this reached exactly one of them.
    #[test]
    fn a_fetch_target_outranks_the_belief_that_used_to_cause_the_beeline() {
        let nav = open_room();
        let dt = 1.0 / 60.0;
        let start = Vec3::new(10.0, 0.05, 10.0);
        let gun = Vec3::new(1.0, 0.05, 10.0);
        let player = Vec3::new(19.0, 0.05, 10.0);

        for (label, utility, pd) in [("utility", true, false), ("fsm", false, false), ("pd", false, true)] {
            let mut physics = PhysicsWorld::new();
            let collider = physics.add_enemy_collider(start, 0.24, 0.48);
            let mut e = Enemy::new(start, player);
            e.set_utility(utility);
            let tuning = AiTuning {
                mode: if pd { AiMode::Pd } else { AiMode::Ours },
                ..AiTuning::default()
            };
            // The hunter cannot see the player (shopping suppression) but DOES believe it
            // knows where they are — the state a gunshot leaves it in.
            e.set_detectable(false);
            e.hear_noise(player);
            assert_eq!(e.last_known, Some(player), "the precondition: a live belief");
            e.set_fetch_target(Some(gun));

            // Stop on arrival rather than running a fixed window: nothing here grants
            // the pickup (there is no `World`), so a longer run would hit the
            // [`FETCH_STALL`] give-up — which is its own test.
            let mut arrived = false;
            for _ in 0..300 {
                drive(&mut e, dt, player, 4.8, tuning, false, &nav, &mut physics, false, collider);
                if e.dist_to(gun) < 1.0 {
                    arrived = true;
                    break;
                }
            }
            assert_eq!(e.state, AiState::Fetch, "{label}: it stopped shopping");
            assert!(
                arrived,
                "{label}: it never reached the gun — it walked from {start:?} to {:?}, the gun \
                 is at {gun:?} and the player at {player:?}",
                e.pos
            );
        }
    }

    /// A hunter must never become a **statue** next to something it cannot pick up. The
    /// nav grid steers its centre to a cell, so a pickup on a ledge or in an unreachable
    /// pocket leaves it parked short of the collect radius; after [`FETCH_STALL`] it
    /// writes that pickup off and goes back to searching for [`FETCH_RETRY`].
    ///
    /// Modelled by leaving the fetch target where the hunter already stands, which is
    /// what "arrived and nothing happened" looks like from inside the behaviour.
    #[test]
    fn a_pickup_that_never_arrives_is_given_up_on_rather_than_stood_next_to() {
        let nav = open_room();
        let dt = 1.0 / 60.0;
        let start = Vec3::new(10.0, 0.05, 10.0);
        let mut physics = PhysicsWorld::new();
        let collider = physics.add_enemy_collider(start, 0.24, 0.48);
        let mut e = Enemy::new(start, Vec3::NEG_Z);
        e.set_utility(true);
        e.set_detectable(false);
        e.set_fetch_target(Some(start)); // standing on it, and it never grants
        e.assign_search_target(Vec3::new(1.0, 0.05, 10.0));

        let mut gave_up_at = None;
        for f in 0..(60.0 * (FETCH_STALL + 1.0)) as usize {
            drive(&mut e, dt, Vec3::new(19.0, 0.05, 10.0), 4.8, AiTuning::default(),
                  false, &nav, &mut physics, false, collider);
            if gave_up_at.is_none() && e.state != AiState::Fetch {
                gave_up_at = Some(f as f32 * dt);
            }
        }
        let t = gave_up_at.expect("the hunter stood at an uncollectable pickup forever");
        assert!(
            (FETCH_STALL..FETCH_STALL + 0.5).contains(&t),
            "gave up after {t:.1}s; the clock is {FETCH_STALL:.0}s"
        );
        // And it goes back to hunting rather than standing there, then re-asks later.
        assert_eq!(e.state, AiState::Search, "it gave up into {:?}", e.state);
        assert!(e.fetch_block > 0.0, "the retry suppression is what stops it re-committing");
    }
}
