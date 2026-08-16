//! Perfect Dark's 13 simulant **personality types** (`BOTTYPE_*`,
//! `pd-decomp/src/include/constants.h:374`).
//!
//! The structural point, and the reason this is a separate axis from
//! [`super::difficulty`]: personality changes **target selection and goals only,
//! never accuracy**. A PeaceSim on Dark is a lethal shot that refuses to start
//! fights; a KazeSim on Meat charges you and misses. PD keeps one shared
//! targeting algorithm and layers personality on as small **veto predicates** —
//! `bot_passes_peace_check` (`bot.c:1537`) and `bot_passes_coward_check`
//! (`bot.c:1557`) are ~15 lines each.
//!
//! That maps directly onto a scorer, so each type here is a veto plus a small set
//! of weights rather than a bespoke behaviour tree.

/// How dangerous a candidate looks, so personalities that care about relative
/// strength (`COWARD`, `PREY`, `JUDGE`) have something to compare. PD gets this
/// from `botinv_score_weapon`, which scores an opponent's weapon with the same
/// routine it scores its own.
#[derive(Clone, Copy, Debug, Default)]
pub struct Threat {
    /// Weapon score, arbitrary units but shared between bot and candidate.
    pub weapon_score: f32,
    /// Health remaining, 0..1.
    pub health_frac: f32,
    /// Seconds since this candidate spawned — PreySim hunts the freshly spawned.
    pub since_spawn: f32,
    /// Current score / kill count, for JudgeSim.
    pub score: f32,
}

/// The margin by which a CowardSim must out-gun a candidate before it will engage
/// (`bot_passes_coward_check`, `bot.c:1557`).
const COWARD_MARGIN: f32 = 30.0;
/// A PreySim treats anyone under this health fraction, or newer than
/// [`PREY_FRESH_SECS`], as weak enough to prioritise.
const PREY_HEALTH_FRAC: f32 = 0.5;
const PREY_FRESH_SECS: f32 = 5.0;
/// Bonus applied to a candidate a personality actively wants. Large enough to
/// reorder the distance-sorted candidate list, small enough that an adjacent
/// target still wins over one across the map.
const PREFERENCE_BONUS: f32 = 40.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BotType {
    /// No modifier — the baseline.
    General,
    /// Collects weapons but will not engage.
    Peace,
    /// Wants full shield before fighting. We have no shield system, so this
    /// degrades to General; kept so the enum matches PD and a future armour
    /// system can wire it up.
    Shield,
    /// Prefers explosive weapons.
    Rocket,
    /// Does not keep distance.
    Kaze,
    /// Fists only.
    Fist,
    /// Targets the newly spawned, poorly armed, or low health.
    Prey,
    /// Flees unless it out-guns you.
    Coward,
    /// Targets whoever is winning.
    Judge,
    /// Fixes on one player for the whole match.
    Feud,
    /// Moves faster.
    Speed,
    /// Moves slower, double shield.
    Turtle,
    /// Targets whoever last killed it.
    Venge,
}

impl BotType {
    pub const ALL: [BotType; 13] = [
        BotType::General,
        BotType::Peace,
        BotType::Shield,
        BotType::Rocket,
        BotType::Kaze,
        BotType::Fist,
        BotType::Prey,
        BotType::Coward,
        BotType::Judge,
        BotType::Feud,
        BotType::Speed,
        BotType::Turtle,
        BotType::Venge,
    ];

    pub fn name(self) -> &'static str {
        match self {
            BotType::General => "GeneralSim",
            BotType::Peace => "PeaceSim",
            BotType::Shield => "ShieldSim",
            BotType::Rocket => "RocketSim",
            BotType::Kaze => "KazeSim",
            BotType::Fist => "FistSim",
            BotType::Prey => "PreySim",
            BotType::Coward => "CowardSim",
            BotType::Judge => "JudgeSim",
            BotType::Feud => "FeudSim",
            BotType::Speed => "SpeedSim",
            BotType::Turtle => "TurtleSim",
            BotType::Venge => "VengeSim",
        }
    }

    /// Movement speed override (`bot_calculate_max_speed`, `bot.c:1104`).
    ///
    /// PD applies these *instead of* the difficulty scale, not on top of it — a
    /// SpeedSim moves the same whether it is Meat or Dark. Returning `Some` here
    /// means "ignore the difficulty ratio entirely"; the value is expressed
    /// against Normal, matching [`super::difficulty::BotDifficulty::speed_ratio`].
    pub fn speed_override(self) -> Option<f32> {
        match self {
            BotType::Turtle => Some(3.5 / 7.6),
            BotType::Speed => Some(14.0 / 7.6),
            _ => None,
        }
    }

    /// Standoff multiplier — how much of its weapon's preferred range the bot
    /// actually keeps. KazeSim does not keep distance (`KAZE` closes to contact).
    pub fn standoff_mult(self) -> f32 {
        match self {
            BotType::Kaze | BotType::Fist => 0.0,
            _ => 1.0,
        }
    }

    /// Whether this type refuses to fight at all (PeaceSim collects weapons but
    /// will not engage). Distinct from the peace *check*, which is about who is a
    /// legitimate target.
    pub fn pacifist(self) -> bool {
        matches!(self, BotType::Peace)
    }

    /// Prefers explosive weapons when choosing what to carry.
    pub fn prefers_explosives(self) -> bool {
        matches!(self, BotType::Rocket)
    }

    /// Restricted to melee.
    pub fn melee_only(self) -> bool {
        matches!(self, BotType::Fist)
    }
}

/// Everything a personality needs to know about one candidate target to accept,
/// reject or rank it.
#[derive(Clone, Copy, Debug)]
pub struct Candidate {
    /// Stable identity, so FeudSim and VengeSim can fixate.
    pub id: u32,
    pub distance: f32,
    pub in_sight: bool,
    pub alive: bool,
    pub threat: Threat,
    /// Whether the candidate is holding any weapon — a PeaceSim only fights the
    /// armed.
    pub armed: bool,
}

/// The mutable personality memory a simulant carries between ticks.
#[derive(Clone, Copy, Debug, Default)]
pub struct Grudge {
    /// FeudSim's chosen nemesis for the whole match, once picked.
    pub feud_target: Option<u32>,
    /// VengeSim's most recent killer.
    pub last_killer: Option<u32>,
}

/// The veto: may this bot target this candidate at all?
///
/// Mirrors the checks `bot_choose_general_target` runs while validating a target
/// (`bot.c:1589`). Returning false does not mean "prefer someone else" — it means
/// this candidate is not a legal target for this personality.
pub fn passes_vetoes(ty: BotType, own: &Threat, cand: &Candidate, grudge: &Grudge) -> bool {
    if !cand.alive {
        return false;
    }
    match ty {
        // `bot_passes_peace_check` (bot.c:1537) — a PeaceSim only fights the armed.
        BotType::Peace => cand.armed,
        // `bot_passes_coward_check` (bot.c:1557) — declines unless it leads by a
        // clear margin. PD only applies this to an out-of-sight target (an
        // in-sight one is already a fight), so a cornered CowardSim still fights.
        BotType::Coward => {
            cand.in_sight || own.weapon_score >= cand.threat.weapon_score + COWARD_MARGIN
        }
        // Once a FeudSim has picked its nemesis, nobody else is a legal target.
        BotType::Feud => grudge.feud_target.map_or(true, |t| t == cand.id),
        _ => true,
    }
}

/// Preference score added on top of the shared distance ordering. Higher wins.
///
/// This is the "score adjustments" half of the pattern — where a veto says *may
/// not*, this says *would rather*.
pub fn preference(ty: BotType, cand: &Candidate, grudge: &Grudge) -> f32 {
    match ty {
        BotType::Prey => {
            let weak = cand.threat.health_frac < PREY_HEALTH_FRAC
                || cand.threat.since_spawn < PREY_FRESH_SECS
                || cand.threat.weapon_score <= 0.0;
            if weak {
                PREFERENCE_BONUS
            } else {
                0.0
            }
        }
        // JudgeSim targets whoever is winning, so score *is* the preference.
        BotType::Judge => cand.threat.score,
        BotType::Venge => {
            if grudge.last_killer == Some(cand.id) {
                PREFERENCE_BONUS
            } else {
                0.0
            }
        }
        _ => 0.0,
    }
}

/// Give the personality a chance to latch onto a target it should remember.
/// FeudSim fixes on the first legal target it ever picks and never changes.
pub fn note_target(ty: BotType, chosen: Option<u32>, grudge: &mut Grudge) {
    if ty == BotType::Feud && grudge.feud_target.is_none() {
        grudge.feud_target = chosen;
    }
}

impl Threat {
    /// Convenience for wrapping a bot's own stats to compare against candidates.
    /// The coward check runs the bot and its candidate through the same scoring,
    /// exactly as PD reuses `botinv_score_weapon` for both sides.
    pub fn of(weapon_score: f32, health_frac: f32) -> Self {
        Threat { weapon_score, health_frac, since_spawn: 0.0, score: 0.0 }
    }
}
