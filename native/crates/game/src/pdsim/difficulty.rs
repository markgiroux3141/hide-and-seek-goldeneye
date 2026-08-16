//! Perfect Dark's `botdifficulty` tuning table, converted to SI units.
//!
//! Ported from `pd-decomp/src/game/bot.c:87` (`g_BotDifficulties`), with the
//! original authors' field notes at `bot.c:56-85`. See `DESIGN_PD_SIMULANT_AI.md`
//! §3 for why this table is the whole lethality axis.
//!
//! The N64 source stores times in 60ths of a second and angles through
//! `BADDTOR2`, which converts degrees to radians using Rare's slightly-wrong pi
//! (`M_BADPI = 3.141092641`, `include/math.h:5`). Both conversions happen here so
//! everything downstream is plain seconds and radians. The bad-pi constant is
//! preserved rather than corrected: it is a 0.016% difference that changes
//! nothing perceptually, but keeping it means the numbers can be diffed against
//! the decompilation without a fudge factor.
//!
//! Deliberately not ported: `dizzyamount` (tranquilliser degradation — we have no
//! tranq weapon).

/// Rare's pi (`M_BADPI`). See the module docs.
const BAD_PI: f32 = 3.141_092_6;

/// `BADDTOR2` — degrees to radians through Rare's pi.
const fn deg(d: f32) -> f32 {
    d * BAD_PI / 180.0
}

/// `TICKS(n)` on NTSC is `n`, and a tick is a 60th of a second.
const fn ticks(t: f32) -> f32 {
    t / 60.0
}

/// The six real difficulty tiers (`BOTDIFF_*`, `constants.h:347`). PD's seventh
/// value, `DISABLED`, is an off switch rather than a tier and has no row here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BotDifficulty {
    Meat,
    Easy,
    Normal,
    Hard,
    Perfect,
    Dark,
}

impl BotDifficulty {
    pub const ALL: [BotDifficulty; 6] = [
        BotDifficulty::Meat,
        BotDifficulty::Easy,
        BotDifficulty::Normal,
        BotDifficulty::Hard,
        BotDifficulty::Perfect,
        BotDifficulty::Dark,
    ];

    pub fn name(self) -> &'static str {
        match self {
            BotDifficulty::Meat => "MeatSim",
            BotDifficulty::Easy => "EasySim",
            BotDifficulty::Normal => "NormalSim",
            BotDifficulty::Hard => "HardSim",
            BotDifficulty::Perfect => "PerfectSim",
            BotDifficulty::Dark => "DarkSim",
        }
    }

    pub fn tuning(self) -> BotTuning {
        TABLE[self as usize]
    }

    /// Base movement speed multiplier for this tier (`bot_calculate_max_speed`,
    /// `bot.c:1096`). Returned as a ratio against Normal (PD's default tier) so it
    /// can scale our existing m/s locomotion constants rather than importing PD's
    /// world units.
    pub fn speed_ratio(self) -> f32 {
        let raw = match self {
            BotDifficulty::Meat => 5.0,
            BotDifficulty::Easy => 6.2,
            BotDifficulty::Normal => 7.6,
            BotDifficulty::Hard => 9.4,
            // PD gives Perfect and Dark the same speed.
            BotDifficulty::Perfect | BotDifficulty::Dark => 11.2,
        };
        raw / 7.6
    }
}

/// One row of `g_BotDifficulties`, in seconds and radians.
#[derive(Clone, Copy, Debug)]
pub struct BotTuning {
    /// Time the bot waits between *seeing* the target and shooting (s). Has a
    /// cooldown rather than a reset, so a brief line-of-sight break barely helps.
    pub shoot_delay: f32,
    /// Lower bound of the per-tick angular convergence rate (rad), before the
    /// zero-progress scaling.
    pub min_zero_speed: f32,
    /// Upper bound of the same (rad).
    pub max_zero_speed: f32,
    /// How long a full zero onto the target takes (s).
    pub zero_time: f32,
    /// Multiplier on how fast natural turning *un-zeroes* the aim. From 10 (Meat,
    /// crippled by turning) down to 0 (Dark, unaffected).
    pub turn_unzero_mult: f32,
    /// Floor on `max_zero_speed` when the target is cloaked (rad). Kept for
    /// completeness — we have no cloak, so nothing drives it yet.
    pub zero_cloak_speed: f32,
    /// General floor on `max_zero_speed` (rad), so a bot starting a zero always has
    /// some convergence rate available. This is also what leaves a permanent
    /// residual aim wobble on every tier below Perfect.
    pub force_zero_min_speed: f32,
}

/// `g_BotDifficulties` (`bot.c:87`), row order matching [`BotDifficulty`].
static TABLE: [BotTuning; 6] = [
    // meat
    BotTuning {
        shoot_delay: ticks(90.0),
        min_zero_speed: deg(15.0),
        max_zero_speed: deg(30.0),
        zero_time: ticks(600.0),
        turn_unzero_mult: 10.0,
        zero_cloak_speed: deg(40.0),
        force_zero_min_speed: deg(20.0),
    },
    // easy
    BotTuning {
        shoot_delay: ticks(60.0),
        min_zero_speed: deg(7.0),
        max_zero_speed: deg(14.0),
        zero_time: ticks(360.0),
        turn_unzero_mult: 10.0,
        zero_cloak_speed: deg(28.5),
        force_zero_min_speed: deg(8.0),
    },
    // normal
    BotTuning {
        shoot_delay: ticks(30.0),
        min_zero_speed: deg(4.0),
        max_zero_speed: deg(8.0),
        zero_time: ticks(180.0),
        turn_unzero_mult: 4.0,
        zero_cloak_speed: deg(20.0),
        force_zero_min_speed: deg(5.0),
    },
    // hard
    BotTuning {
        shoot_delay: ticks(15.0),
        min_zero_speed: deg(1.5),
        max_zero_speed: deg(4.0),
        zero_time: ticks(90.0),
        turn_unzero_mult: 2.0,
        zero_cloak_speed: deg(14.0),
        force_zero_min_speed: deg(2.0),
    },
    // perfect
    BotTuning {
        shoot_delay: 0.0,
        min_zero_speed: 0.0,
        max_zero_speed: deg(2.0),
        zero_time: ticks(45.0),
        turn_unzero_mult: 1.0,
        zero_cloak_speed: deg(10.0),
        force_zero_min_speed: 0.0,
    },
    // dark
    BotTuning {
        shoot_delay: 0.0,
        min_zero_speed: 0.0,
        max_zero_speed: 0.0,
        zero_time: 0.0,
        turn_unzero_mult: 0.0,
        zero_cloak_speed: deg(8.0),
        force_zero_min_speed: 0.0,
    },
];

/// Map our continuous 0..=10 difficulty dial onto the table by interpolating
/// between adjacent PD rows.
///
/// PD's six tiers are coarse; our dial is not. Interpolating rather than
/// bucketing keeps the dial's existing feel of a smooth ramp, and every field in
/// the table is monotonic across tiers, so a blend between two rows is always a
/// sensible intermediate rather than a nonsense mix.
pub fn tuning_for_dial(dial: f32, max_dial: f32) -> BotTuning {
    let t = (dial / max_dial.max(1e-6)).clamp(0.0, 1.0) * (BotDifficulty::ALL.len() - 1) as f32;
    let lo = t.floor() as usize;
    let hi = (lo + 1).min(BotDifficulty::ALL.len() - 1);
    let f = t - lo as f32;
    let a = TABLE[lo];
    let b = TABLE[hi];
    let mix = |x: f32, y: f32| x + (y - x) * f;
    BotTuning {
        shoot_delay: mix(a.shoot_delay, b.shoot_delay),
        min_zero_speed: mix(a.min_zero_speed, b.min_zero_speed),
        max_zero_speed: mix(a.max_zero_speed, b.max_zero_speed),
        zero_time: mix(a.zero_time, b.zero_time),
        turn_unzero_mult: mix(a.turn_unzero_mult, b.turn_unzero_mult),
        zero_cloak_speed: mix(a.zero_cloak_speed, b.zero_cloak_speed),
        force_zero_min_speed: mix(a.force_zero_min_speed, b.force_zero_min_speed),
    }
}

/// The nearest discrete tier to a dial position — for labelling the HUD, where
/// "HardSim" reads better than a pile of interpolated constants.
pub fn tier_for_dial(dial: f32, max_dial: f32) -> BotDifficulty {
    let t = (dial / max_dial.max(1e-6)).clamp(0.0, 1.0) * (BotDifficulty::ALL.len() - 1) as f32;
    BotDifficulty::ALL[(t.round() as usize).min(BotDifficulty::ALL.len() - 1)]
}
