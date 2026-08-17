//! Perfect Dark's weapon table — GENERATED, do not hand-edit.
//!
//! Regenerate with:
//!
//! ```text
//! python tools/pd-assets/pd_weapons.py rust \
//!     native/crates/game/src/combat/pd_weapons.rs
//! ```
//!
//! Every row carries the `invitems.c` line it came from, the same provenance
//! discipline [`super::attack_anim`] uses. The generator parses the decomp rather
//! than trusting a transcription, because this is ~33 weapons x 2 functions x ~20
//! bare numeric literals and a one-column slip in that is invisible.
//!
//! # What PD authors that we were guessing
//!
//! * **Two functions per weapon.** `weapondef.functions[2]`. Ours had one fire
//!   mode per gun; every PD weapon has a primary and a secondary, and they are
//!   frequently different *kinds* (the SuperDragon is an automatic plus a grenade
//!   launcher; the Reaper is an automatic plus a melee grind).
//! * **Viewmodel placement.** `muzzlez/posx/posy/posz/sway` is the authored
//!   version of what `weapon-config.json` hand-tuned and [`super::config`] bakes
//!   in as `model_offset` / `pivot_offset` / `muzzle_offset`.
//! * **Engagement distance, per function.** [`PD_DIST_BANDS`] indexed by
//!   `PdAi::band_pri` / `band_sec`. Our `standoff_for` derived a standoff from a
//!   guessed range with a 0.6 fudge factor; PD authored a min/max per weapon AND
//!   per function.
//! * **Automatics spin up.** `initial_rpm` -> `max_rpm`, against our flat
//!   `fire_cooldown`.
//!
//! # Three things measured, not assumed
//!
//! 1. **`WEAPONFLAG_AICANUSE` is not a gun filter.** The handoff described it as
//!    saying "exactly which guns an enemy may hold". It is set on all 64 real
//!    weapons and absent only from the 20 non-weapons (keycards, briefcases, bare
//!    projectile items) — so it gates *items*, and every MP gun is AI-usable. The
//!    real per-weapon AI data is `g_BotWeaponConfigs` ([`PdAi`]), which scores
//!    each function separately.
//! 2. **1 PD damage unit = 25.0 of our HP** ([`PD_DAMAGE_TO_HP`]). Derived from
//!    two independent facts agreeing, not fitted: a PD guard has `maxdamage = 4`
//!    (`chr.c:1127`) and the Falcon 2 does `damage = 1`, so four body shots kill;
//!    our [`crate::enemy::ENEMY_HEALTH`] is 100 and our PP7 does 25, so also four.
//! 3. **PD world units are centimetres** ([`PD_CM_TO_M`]). Independently pinned
//!    in `tools/pd-assets/pd_pose.py`, whose derivation cites the very numbers in
//!    `g_BotDistConfigs` ("a bot follows within 300 (3 m)").
//!
//! Damage and spread stay in **PD units** here — this file is the transcription,
//! and converting on the way in would bake an interpretation into the data. The
//! consumers convert.

#![allow(dead_code)] // the table is transcribed whole; consumers land per milestone

/// Multiplier from a PD damage number to our HP scale. See the module docs — this
/// is derived from shots-to-kill agreeing on both sides, not tuned.
pub const PD_DAMAGE_TO_HP: f32 = 25.0;

/// PD world units are centimetres.
pub const PD_CM_TO_M: f32 = 0.01;

/// PD ticks are 60ths of a second (the `*60` field suffix throughout the decomp).
pub const PD_TICKS_PER_SEC: f32 = 60.0;


// ─── `FUNCFLAG_*` (constants.h) — behaviour flags on a firing function ──────
// Transcribed whole, including the ones nothing consumes yet: porting a table
// means porting its filters too, and a flag that is absent cannot later be
// noticed as missing.

pub const FUNCFLAG_00000001: u32 = 0x00000001;
pub const FUNCFLAG_BURST3: u32 = 0x00000002;
pub const FUNCFLAG_BURST50: u32 = 0x00000020;
pub const FUNCFLAG_NOAUTOAIM: u32 = 0x00000040;
pub const FUNCFLAG_STICKTOWALL: u32 = 0x00000100;
pub const FUNCFLAG_MAKEDIZZY: u32 = 0x00000200;
pub const FUNCFLAG_DISARM: u32 = 0x00000400;
pub const FUNCFLAG_FLYBYWIRE: u32 = 0x00000800;
pub const FUNCFLAG_BURST2: u32 = 0x00001000;
pub const FUNCFLAG_NOMUZZLEFLASH: u32 = 0x00002000;
pub const FUNCFLAG_EXPLOSIVESHELLS: u32 = 0x00004000;
pub const FUNCFLAG_BLUNTIMPACT: u32 = 0x00008000;
pub const FUNCFLAG_NOSTUN: u32 = 0x00010000;
pub const FUNCFLAG_BURST5: u32 = 0x00020000;
pub const FUNCFLAG_DISCARDWEAPON: u32 = 0x00040000;
pub const FUNCFLAG_THREATDETECTOR: u32 = 0x00080000;
pub const FUNCFLAG_AUTOSWITCHUNSELECTABLE: u32 = 0x00100000;
pub const FUNCFLAG_PSYCHOSIS: u32 = 0x00200000;
pub const FUNCFLAG_00400000: u32 = 0x00400000;
pub const FUNCFLAG_CALCULATETRAJECTORY: u32 = 0x00800000;
pub const FUNCFLAG_PROJECTILE_POWERED: u32 = 0x08000000;
pub const FUNCFLAG_10000000: u32 = 0x10000000;
pub const FUNCFLAG_20000000: u32 = 0x20000000;
pub const FUNCFLAG_HOMINGROCKET: u32 = 0x40000000;
pub const FUNCFLAG_PROJECTILE_LIGHTWEIGHT: u32 = 0x80000000;


/// Which of PD's seven `funcdef` subtypes a firing function is
/// (`INVENTORYFUNCTYPE_*`, `types.h:2910-3010`). Our [`super::FireKind`] had
/// three cases; this is the full set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdFuncKind {
    /// `funcdef_shootsingle` — one round per pull.
    Single,
    /// `funcdef_shootauto` — held fire that spins up from `initial_rpm` to `max_rpm`.
    Auto,
    /// `funcdef_shootprojectile` — launches a travelling round.
    Projectile,
    /// `funcdef_throw` — lobbed (grenades, mines, the Laptop's sentry deploy).
    Throw,
    /// `funcdef_melee` — contact damage inside `melee_range`.
    Melee,
    /// `funcdef_special` — a scripted behaviour (cloak, crouch, detonate).
    Special,
    /// `funcdef_device` — a held gadget rather than a weapon (scanners).
    Device,
}

/// One firing function. Fields not meaningful for the kind are zero — PD's
/// subtypes are a struct hierarchy and this is their union, flattened.
#[derive(Clone, Copy, Debug)]
pub struct PdFunc {
    /// The authored in-game label, e.g. `"Single Shot"`, `"Grenade Launcher"`.
    pub label: &'static str,
    pub kind: PdFuncKind,
    /// `FUNCFLAG_*` bits — see the constants above.
    pub flags: u32,
    /// Damage in **PD units**; multiply by [`PD_DAMAGE_TO_HP`] for our scale.
    pub damage: f32,
    /// PD's per-shot cone width, in its own units (see [`crate::pdsim::spread`]).
    pub spread: f32,
    /// Ticks (60ths) before the weapon may fire again.
    pub recovery60: i32,
    /// How many bodies a round passes through.
    pub penetration: u32,
    /// `Auto` only: the rate the trigger-pull starts at, and what it winds up to.
    pub initial_rpm: f32,
    pub max_rpm: f32,
    /// `Projectile` only: launch speed and fuse (ticks).
    pub projectile_speed: f32,
    pub projectile_timer60: i32,
    /// `Melee` only: reach, in PD centimetres.
    pub melee_range: f32,
    /// Viewmodel recoil: kick-back distance (PD centimetres) and muzzle rise
    /// (PD's own angle units). The authored counterpart of our `recoil_z` /
    /// `recoil_rot`, which are two shared constants across all 24 GE weapons.
    pub recoil_dist: f32,
    pub recoil_angle: f32,
    /// `invitems.c` line of the `funcdef` this row came from.
    pub source: &'static str,
}

impl PdFunc {
    /// An inert function, for the one MP entry that has none at all
    /// (`MPWEAPON_SHIELD` — `invitem_shieldtechitem` carries no `functions[2]`).
    /// Keeping `primary` non-optional means the 33 guns never unwrap; this is the
    /// single row that needs a stand-in, and it is `equipment_only` anyway.
    pub const INERT: PdFunc = PdFunc {
        label: "",
        kind: PdFuncKind::Device,
        flags: 0,
        damage: 0.0,
        spread: 0.0,
        recovery60: 0,
        penetration: 0,
        initial_rpm: 0.0,
        max_rpm: 0.0,
        projectile_speed: 0.0,
        projectile_timer60: 0,
        melee_range: 0.0,
        recoil_dist: 0.0,
        recoil_angle: 0.0,
        source: "",
    };

    /// Damage on our 100-HP scale.
    pub fn damage_hp(&self) -> f32 {
        self.damage * PD_DAMAGE_TO_HP
    }

    /// Seconds between shots at the *sustained* rate: an automatic's `max_rpm`,
    /// otherwise its `recovery60`. This is the honest analogue of our flat
    /// `fire_cooldown`; the spin-up needs the runtime to track a trigger hold.
    pub fn sustained_cooldown(&self) -> f32 {
        if self.kind == PdFuncKind::Auto && self.max_rpm > 0.0 {
            60.0 / self.max_rpm
        } else if self.recovery60 > 0 {
            self.recovery60 as f32 / PD_TICKS_PER_SEC
        } else {
            0.0
        }
    }

    pub fn has_flag(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    /// `recoildist` as metres. PD's values run 0-40ish in centimetres, which is a
    /// viewmodel kick and not a world distance.
    pub fn recoildist_m(&self) -> f32 {
        self.recoil_dist * PD_CM_TO_M
    }

    /// `recoilangle` as radians. PD stores whole-ish degrees here (the Falcon 2's
    /// 15, the Magnum's larger), so this is a degree conversion.
    pub fn recoilangle_rad(&self) -> f32 {
        self.recoil_angle.to_radians()
    }
}

/// A bot engagement band (`g_BotDistConfigs`, `botcmd.c:29`): the min and max
/// distance a hunter wants to be at while attacking. Indexed by
/// [`PdAi::band_pri`] / [`PdAi::band_sec`].
#[derive(Clone, Copy, Debug)]
pub struct PdDistBand {
    pub name: &'static str,
    pub min_m: f32,
    pub max_m: f32,
}

/// The AI's authored view of a weapon (`g_BotWeaponConfigs`, `botinv.c:21`).
/// `score_*` is how much a bot wants that function — this is what makes
/// "primary or secondary?" a data question rather than our judgement.
#[derive(Clone, Copy, Debug)]
pub struct PdAi {
    pub score_pri: u8,
    pub score_sec: u8,
    pub dual_pri: u8,
    pub dual_sec: u8,
    /// Index into [`PD_DIST_BANDS`].
    pub band_pri: u8,
    pub band_sec: u8,
    pub source: &'static str,
}

/// `weapondef`'s viewmodel placement — the authored version of the values
/// `weapon-config.json` was hand-tuned to. In PD centimetres.
#[derive(Clone, Copy, Debug)]
pub struct PdView {
    pub muzzlez: f32,
    pub posx: f32,
    pub posy: f32,
    pub posz: f32,
    pub sway: f32,
}

/// One Perfect Dark weapon, as the MP set defines it.
#[derive(Clone, Copy, Debug)]
pub struct PdWeapon {
    /// `MPWEAPON_*` index — the multiplayer slot, and this table's stable id.
    pub mp_index: u8,
    /// The authored name, e.g. `"Falcon 2"`, `"FarSight XR-20"`.
    pub name: &'static str,
    /// First-person model, relative to PD's `files/` (`weapondef.hi_model`).
    pub fp_model: &'static str,
    /// Third-person model an enemy holds — the one carrying `CHRGUNFIRE`.
    pub tp_model: &'static str,
    /// The exported first-person GLB, relative to `native/assets/weapons/` so it
    /// drops straight into the same slot as a GoldenEye `WeaponStats::gun_path`.
    pub fp_glb: &'static str,
    /// The exported third-person GLB — what a hunter holds. Unlike the GoldenEye
    /// guns this needs no hand-stripping: PD's `chr*` models are the third-person
    /// weapon alone, so the `enemy-weapon-hand-artifact` cannot arise.
    pub tp_glb: &'static str,
    /// Authored muzzle / shot origin on the third-person model, in engine units.
    /// From `CHRGUNFIRE` where PD authors one, else PD's own grip fallback — see
    /// [`Self::muzzle_is_authored`].
    pub tp_muzzle: [f32; 3],
    /// True when [`Self::tp_muzzle`] came from a real `CHRGUNFIRE` node; false
    /// when it is `chr_get_gun_pos`'s `MODELPART_0001` grip fallback (17 of 33).
    pub muzzle_is_authored: bool,
    /// Rounds per magazine (`ammodef.clipsize`); 0 when the weapon has no clip.
    pub clip_size: i32,
    /// Rounds an MP match hands out (`mpweapon.priammoqty`).
    pub ammo_qty: u32,
    pub primary: PdFunc,
    /// PD's defining feature. `None` only for the equipment entries.
    pub secondary: Option<PdFunc>,
    pub view: PdView,
    pub ai: PdAi,
    /// `WEAPONFLAG_ONEHANDED` — guards carry it in one hand (our pistol class).
    pub one_handed: bool,
    /// `WEAPONFLAG_DUALWIELD` — may be held in both hands.
    pub dual_wield: bool,
    /// `WEAPONFLAG_AICANUSE`. True for every gun — see the module docs; kept
    /// because its *absence* is meaningful for non-weapons.
    pub ai_can_use: bool,
    /// True for the four MP entries with no gameplay system to attach to
    /// (X-Ray, Cloak, Combat Boost, Shield) — excluded from the port scope.
    pub equipment_only: bool,
    /// `invitems.c` line of this `weapondef`.
    pub source: &'static str,
}

impl PdWeapon {
    /// The function a `secondary` request resolves to, falling back to the
    /// primary so a caller never has to special-case a one-function entry.
    pub fn function(&self, secondary: bool) -> &PdFunc {
        if secondary {
            self.secondary.as_ref().unwrap_or(&self.primary)
        } else {
            &self.primary
        }
    }

    /// The engagement band for a function, as metres.
    pub fn band_m(&self, secondary: bool) -> (f32, f32) {
        let i = if secondary { self.ai.band_sec } else { self.ai.band_pri } as usize;
        match PD_DIST_BANDS.get(i) {
            Some(b) => (b.min_m, b.max_m),
            None => (0.0, 0.0),
        }
    }
}


/// `g_BotDistConfigs` (`botcmd.c:29`), converted to metres.
pub const PD_DIST_BANDS: [PdDistBand; 8] = [
    PdDistBand { name: "BOTDISTCFG_CLOSE", min_m: 0.0, max_m: 1.2 },
    PdDistBand { name: "BOTDISTCFG_PISTOL", min_m: 3.0, max_m: 4.5 },
    PdDistBand { name: "BOTDISTCFG_DEFAULT", min_m: 3.0, max_m: 6.0 },
    PdDistBand { name: "BOTDISTCFG_SHOOTEXPLOSIVE", min_m: 6.0, max_m: 12.0 },
    PdDistBand { name: "BOTDISTCFG_KAZE", min_m: 1.5, max_m: 2.5 },
    PdDistBand { name: "BOTDISTCFG_FARSIGHT", min_m: 10.0, max_m: 20.0 },
    PdDistBand { name: "BOTDISTCFG_FOLLOW", min_m: 0.0, max_m: 2.5 },
    PdDistBand { name: "BOTDISTCFG_THROWEXPLOSIVE", min_m: 4.5, max_m: 7.0 },
];


/// One row of `g_ExplosionTypes` (`explosions.c:41`). The two fields our single
/// spherical [`super::Explosion`] had no equivalent for:
///
/// * `blast_radius_m` vs `damage_radius_m` — they differ by up to 2x, so the
///   visible fireball is much smaller than the lethal volume.
/// * `propagation_rate` + `duration_s` — a PD blast keeps applying while the
///   radius grows from blast to damage, instead of resolving in one instant.
///
/// The falloff those feed is NOT our linear sphere; see `super::explosives`.
#[derive(Clone, Copy, Debug)]
pub struct PdExplosion {
    pub name: &'static str,
    pub index: u8,
    pub blast_radius_m: f32,
    pub damage_radius_m: f32,
    pub inner_size_m: f32,
    /// Seconds the explosion lives (and grows) for.
    pub duration_s: f32,
    pub propagation_rate: i32,
    /// Damage scale in PD units — the *peak* a chr takes is `damage * 8.0`
    /// (`explosions.c:967`), before falloff.
    pub damage: f32,
    pub source: &'static str,
}

impl PdExplosion {
    /// Peak damage at the centre, on our HP scale. `chr_damage_by_explosion` is
    /// handed `minfrac * damage * 8.0` with `minfrac == 1` dead centre.
    pub fn peak_damage_hp(&self) -> f32 {
        self.damage * 8.0 * PD_DAMAGE_TO_HP
    }
}

pub const PD_EXPLOSIONS: [PdExplosion; 26] = [
    PdExplosion { name: "EXPLOSIONTYPE_NONE", index: 0, blast_radius_m: 0.0, damage_radius_m: 0.0, inner_size_m: 0.001, duration_s: 0.016666667, propagation_rate: 1, damage: 0.0, source: "explosions.c:40" },
    PdExplosion { name: "EXPLOSIONTYPE_BULLETHOLE", index: 1, blast_radius_m: 0.0, damage_radius_m: 0.0, inner_size_m: 0.01, duration_s: 0.5, propagation_rate: 1, damage: 0.0, source: "explosions.c:41" },
    PdExplosion { name: "EXPLOSIONTYPE_EYESPY", index: 2, blast_radius_m: 0.5, damage_radius_m: 0.5, inner_size_m: 0.3, duration_s: 0.66666667, propagation_rate: 1, damage: 0.125, source: "explosions.c:42" },
    PdExplosion { name: "EXPLOSIONTYPE_LAPTOP", index: 3, blast_radius_m: 1.0, damage_radius_m: 1.0, inner_size_m: 0.5, duration_s: 0.75, propagation_rate: 1, damage: 0.5, source: "explosions.c:43" },
    PdExplosion { name: "EXPLOSIONTYPE_A51TABLE", index: 4, blast_radius_m: 1.3, damage_radius_m: 2.4, inner_size_m: 1.0, duration_s: 1.0, propagation_rate: 2, damage: 1.0, source: "explosions.c:44" },
    PdExplosion { name: "EXPLOSIONTYPE_FRTARGET", index: 5, blast_radius_m: 1.6, damage_radius_m: 2.8, inner_size_m: 1.5, duration_s: 1.0, propagation_rate: 2, damage: 2.0, source: "explosions.c:45" },
    PdExplosion { name: "EXPLOSIONTYPE_6", index: 6, blast_radius_m: 0.4, damage_radius_m: 0.4, inner_size_m: 0.22, duration_s: 1.0, propagation_rate: 1, damage: 0.5, source: "explosions.c:46" },
    PdExplosion { name: "EXPLOSIONTYPE_7", index: 7, blast_radius_m: 0.7, damage_radius_m: 0.7, inner_size_m: 0.35, duration_s: 1.0, propagation_rate: 1, damage: 1.0, source: "explosions.c:47" },
    PdExplosion { name: "EXPLOSIONTYPE_8", index: 8, blast_radius_m: 1.0, damage_radius_m: 1.6, inner_size_m: 0.5, duration_s: 1.0, propagation_rate: 2, damage: 2.0, source: "explosions.c:48" },
    PdExplosion { name: "EXPLOSIONTYPE_9", index: 9, blast_radius_m: 1.3, damage_radius_m: 1.8, inner_size_m: 0.5, duration_s: 1.0, propagation_rate: 2, damage: 2.0, source: "explosions.c:49" },
    PdExplosion { name: "EXPLOSIONTYPE_10", index: 10, blast_radius_m: 0.8, damage_radius_m: 1.6, inner_size_m: 0.7, duration_s: 1.3333333, propagation_rate: 4, damage: 1.0, source: "explosions.c:50" },
    PdExplosion { name: "EXPLOSIONTYPE_11", index: 11, blast_radius_m: 1.0, damage_radius_m: 2.0, inner_size_m: 1.0, duration_s: 1.5, propagation_rate: 1, damage: 2.0, source: "explosions.c:51" },
    PdExplosion { name: "EXPLOSIONTYPE_12", index: 12, blast_radius_m: 1.4, damage_radius_m: 2.8, inner_size_m: 1.5, duration_s: 1.5, propagation_rate: 2, damage: 4.0, source: "explosions.c:52" },
    PdExplosion { name: "EXPLOSIONTYPE_ROCKET", index: 13, blast_radius_m: 2.0, damage_radius_m: 4.0, inner_size_m: 2.0, duration_s: 1.5, propagation_rate: 2, damage: 4.0, source: "explosions.c:53" },
    PdExplosion { name: "EXPLOSIONTYPE_GASBARREL", index: 14, blast_radius_m: 1.5, damage_radius_m: 3.0, inner_size_m: 1.2, duration_s: 2.5, propagation_rate: 4, damage: 4.0, source: "explosions.c:54" },
    PdExplosion { name: "EXPLOSIONTYPE_15", index: 15, blast_radius_m: 0.0, damage_radius_m: 0.0, inner_size_m: 0.01, duration_s: 0.016666667, propagation_rate: 1, damage: 0.0, source: "explosions.c:55" },
    PdExplosion { name: "EXPLOSIONTYPE_16", index: 16, blast_radius_m: 0.0, damage_radius_m: 0.0, inner_size_m: 0.01, duration_s: 0.016666667, propagation_rate: 1, damage: 0.0, source: "explosions.c:56" },
    PdExplosion { name: "EXPLOSIONTYPE_HUGE17", index: 17, blast_radius_m: 22.0, damage_radius_m: 36.0, inner_size_m: 15.0, duration_s: 8.3333333, propagation_rate: 1, damage: 4.0, source: "explosions.c:57" },
    PdExplosion { name: "EXPLOSIONTYPE_BONDEXPLODE", index: 18, blast_radius_m: 4.5, damage_radius_m: 6.4, inner_size_m: 3.0, duration_s: 1.0, propagation_rate: 1, damage: 4.0, source: "explosions.c:58" },
    PdExplosion { name: "EXPLOSIONTYPE_19", index: 19, blast_radius_m: 3.75, damage_radius_m: 6.0, inner_size_m: 2.5, duration_s: 3.0, propagation_rate: 2, damage: 4.0, source: "explosions.c:59" },
    PdExplosion { name: "EXPLOSIONTYPE_20", index: 20, blast_radius_m: 4.5, damage_radius_m: 6.4, inner_size_m: 6.0, duration_s: 1.0, propagation_rate: 1, damage: 4.0, source: "explosions.c:60" },
    PdExplosion { name: "EXPLOSIONTYPE_SDGRENADE", index: 21, blast_radius_m: 1.4, damage_radius_m: 2.7, inner_size_m: 1.0, duration_s: 0.75, propagation_rate: 2, damage: 3.5, source: "explosions.c:61" },
    PdExplosion { name: "EXPLOSIONTYPE_PHOENIX", index: 22, blast_radius_m: 1.0, damage_radius_m: 2.0, inner_size_m: 0.3, duration_s: 0.66666667, propagation_rate: 1, damage: 0.25, source: "explosions.c:62" },
    PdExplosion { name: "EXPLOSIONTYPE_DRAGONBOMBSPY", index: 23, blast_radius_m: 2.2, damage_radius_m: 5.0, inner_size_m: 2.1, duration_s: 1.5, propagation_rate: 2, damage: 4.0, source: "explosions.c:63" },
    PdExplosion { name: "EXPLOSIONTYPE_24", index: 24, blast_radius_m: 2.0, damage_radius_m: 4.0, inner_size_m: 5.0, duration_s: 1.5, propagation_rate: 2, damage: 4.0, source: "explosions.c:64" },
    PdExplosion { name: "EXPLOSIONTYPE_HUGE25", index: 25, blast_radius_m: 10.0, damage_radius_m: 10.0, inner_size_m: 16.0, duration_s: 3.0, propagation_rate: 2, damage: 4.0, source: "explosions.c:65" },
];

/// The Perfect Dark multiplayer arsenal, in `MPWEAPON_*` order.
///
/// This is the whole MP set including the four equipment entries, which
/// carry `equipment_only: true` — they are transcribed so the table matches
/// the source, and filtered by [`pd_guns`].
pub const PD_WEAPONS: [PdWeapon; 37] = [
    // MPWEAPON_FALCON2 — Accurate and trustworthy, this gun is the workhorse of the Institute's
    PdWeapon {
        mp_index: 1,
        name: "Falcon 2",
        fp_model: "guns/falcon2.bin",
        tp_model: "props/chrfalcon2.bin",
        fp_glb: "pd/01-falcon-2-fp.glb",
        tp_glb: "pd/01-falcon-2-tp.glb",
        tp_muzzle: [-143.02885, 0.0, 28.846154],
        muzzle_is_authored: true,
        clip_size: 8,
        ammo_qty: 80,
        primary: PdFunc {
            label: "Single Shot",
            kind: PdFuncKind::Single,
            flags: 0,
            damage: 1.0,
            spread: 1.0,
            recovery60: 16,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 10.0,
            recoil_angle: 15.0,
            source: "invitems.c:484",
        },
        secondary: Some(PdFunc {
            label: "Pistol Whip",
            kind: PdFuncKind::Melee,
            flags: 4301312,
            damage: 0.9,
            spread: 0.0,
            recovery60: 0,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 60.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:528",
        }),
        view: PdView { muzzlez: 2.0, posx: 9.0, posy: -15.7, posz: -23.8, sway: 1.0 },
        ai: PdAi { score_pri: 56, score_sec: 60, dual_pri: 84, dual_sec: 88, band_pri: 1, band_sec: 0, source: "botinv.c:22" },
        one_handed: true,
        dual_wield: true,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:568",
    },
    // MPWEAPON_FALCON2_SILENCER — An upgraded Falcon 2, which has the added benefit of being silent, but
    PdWeapon {
        mp_index: 2,
        name: "Falcon 2 (silencer)",
        fp_model: "guns/falcon2.bin",
        tp_model: "props/chrfalcon2sil.bin",
        fp_glb: "pd/02-falcon-2-silencer-fp.glb",
        tp_glb: "pd/02-falcon-2-silencer-tp.glb",
        tp_muzzle: [-476.03593, 7.58212, 90.897377],
        muzzle_is_authored: false,
        clip_size: 8,
        ammo_qty: 80,
        primary: PdFunc {
            label: "Single Shot",
            kind: PdFuncKind::Single,
            flags: 8192,
            damage: 1.0,
            spread: 1.0,
            recovery60: 16,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 10.0,
            recoil_angle: 15.0,
            source: "invitems.c:506",
        },
        secondary: Some(PdFunc {
            label: "Pistol Whip",
            kind: PdFuncKind::Melee,
            flags: 4301312,
            damage: 0.9,
            spread: 0.0,
            recovery60: 0,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 60.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:528",
        }),
        view: PdView { muzzlez: 1.0, posx: 9.0, posy: -15.7, posz: -23.8, sway: 1.0 },
        ai: PdAi { score_pri: 52, score_sec: 60, dual_pri: 80, dual_sec: 88, band_pri: 1, band_sec: 0, source: "botinv.c:23" },
        one_handed: true,
        dual_wield: true,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:622",
    },
    // MPWEAPON_FALCON2_SCOPE — An upgraded Falcon 2, featuring a 2x magnification scope which allows 
    PdWeapon {
        mp_index: 3,
        name: "Falcon 2 (scope)",
        fp_model: "guns/falcon2.bin",
        tp_model: "props/chrfalcon2scope.bin",
        fp_glb: "pd/03-falcon-2-scope-fp.glb",
        tp_glb: "pd/03-falcon-2-scope-tp.glb",
        tp_muzzle: [-143.02885, 0.0, 28.846154],
        muzzle_is_authored: true,
        clip_size: 8,
        ammo_qty: 80,
        primary: PdFunc {
            label: "Single Shot",
            kind: PdFuncKind::Single,
            flags: 0,
            damage: 1.0,
            spread: 1.0,
            recovery60: 16,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 10.0,
            recoil_angle: 15.0,
            source: "invitems.c:484",
        },
        secondary: Some(PdFunc {
            label: "Pistol Whip",
            kind: PdFuncKind::Melee,
            flags: 4301312,
            damage: 0.9,
            spread: 0.0,
            recovery60: 0,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 60.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:528",
        }),
        view: PdView { muzzlez: 1.0, posx: 9.0, posy: -15.7, posz: -23.8, sway: 1.0 },
        ai: PdAi { score_pri: 60, score_sec: 60, dual_pri: 88, dual_sec: 88, band_pri: 1, band_sec: 0, source: "botinv.c:24" },
        one_handed: true,
        dual_wield: true,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:597",
    },
    // MPWEAPON_MAGSEC4 — A state-of-the-art military pistol, largely used by peacekeeping force
    PdWeapon {
        mp_index: 4,
        name: "MagSec 4",
        fp_model: "guns/leegun1.bin",
        tp_model: "props/chrleegun1.bin",
        fp_glb: "pd/04-magsec-4-fp.glb",
        tp_glb: "pd/04-magsec-4-tp.glb",
        tp_muzzle: [-188.70192, 0.0, 20.432692],
        muzzle_is_authored: true,
        clip_size: 9,
        ammo_qty: 80,
        primary: PdFunc {
            label: "Single Shot",
            kind: PdFuncKind::Single,
            flags: 0,
            damage: 1.1,
            spread: 6.0,
            recovery60: 16,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 5.0,
            recoil_angle: 10.0,
            source: "invitems.c:726",
        },
        secondary: Some(PdFunc {
            label: "3-Round Burst",
            kind: PdFuncKind::Single,
            flags: 2,
            damage: 1.1,
            spread: 10.0,
            recovery60: 16,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 8.0,
            recoil_angle: 12.0,
            source: "invitems.c:748",
        }),
        view: PdView { muzzlez: 2.0, posx: 10.5, posy: -17.2, posz: -26.5, sway: 1.0 },
        ai: PdAi { score_pri: 76, score_sec: 88, dual_pri: 104, dual_sec: 120, band_pri: 1, band_sec: 2, source: "botinv.c:25" },
        one_handed: true,
        dual_wield: true,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:778",
    },
    // MPWEAPON_MAULER — If you see a Skedar coming at you, the chances are it's carrying one o
    PdWeapon {
        mp_index: 5,
        name: "Mauler",
        fp_model: "guns/skpistol.bin",
        tp_model: "props/chrmauler.bin",
        fp_glb: "pd/05-mauler-fp.glb",
        tp_glb: "pd/05-mauler-tp.glb",
        tp_muzzle: [-217.54808, 0.0, 7.2115385],
        muzzle_is_authored: true,
        clip_size: 20,
        ammo_qty: 92,
        primary: PdFunc {
            label: "Single Shot",
            kind: PdFuncKind::Single,
            flags: 0,
            damage: 1.2,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:1202",
        },
        secondary: Some(PdFunc {
            label: "Charge-Up Shot",
            kind: PdFuncKind::Single,
            flags: 0,
            damage: 1.2,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:1224",
        }),
        view: PdView { muzzlez: 1.0, posx: 11.5, posy: -17.5, posz: -20.0, sway: 1.0 },
        ai: PdAi { score_pri: 64, score_sec: 88, dual_pri: 92, dual_sec: 120, band_pri: 1, band_sec: 2, source: "botinv.c:26" },
        one_handed: true,
        dual_wield: true,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:1254",
    },
    // MPWEAPON_PHOENIX — The Maian standard issue sidearm. A flexible gun, the pistol fires sta
    PdWeapon {
        mp_index: 6,
        name: "Phoenix",
        fp_model: "guns/maianpistol.bin",
        tp_model: "props/chrmaianpistol.bin",
        fp_glb: "pd/06-phoenix-fp.glb",
        tp_glb: "pd/06-phoenix-tp.glb",
        tp_muzzle: [-389.57498, 9.6298531, 68.252428],
        muzzle_is_authored: false,
        clip_size: 8,
        ammo_qty: 64,
        primary: PdFunc {
            label: "Single Shot",
            kind: PdFuncKind::Single,
            flags: 0,
            damage: 1.1,
            spread: 3.0,
            recovery60: 16,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 10.0,
            recoil_angle: 15.0,
            source: "invitems.c:1062",
        },
        secondary: Some(PdFunc {
            label: "Explosive Shells",
            kind: PdFuncKind::Single,
            flags: 16384,
            damage: 1.2,
            spread: 5.0,
            recovery60: 16,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 15.0,
            recoil_angle: 25.0,
            source: "invitems.c:1084",
        }),
        view: PdView { muzzlez: 1.0, posx: 9.5, posy: -16.2, posz: -23.0, sway: 1.0 },
        ai: PdAi { score_pri: 72, score_sec: 76, dual_pri: 100, dual_sec: 120, band_pri: 1, band_sec: 2, source: "botinv.c:27" },
        one_handed: true,
        dual_wield: true,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:1114",
    },
    // MPWEAPON_DY357MAGNUM — The dataDyne DY357 is the most powerful handgun in the world. Each rou
    PdWeapon {
        mp_index: 7,
        name: "DY357 Magnum",
        fp_model: "guns/dy357.bin",
        tp_model: "props/chrdy357.bin",
        fp_glb: "pd/07-dy357-magnum-fp.glb",
        tp_glb: "pd/07-dy357-magnum-tp.glb",
        tp_muzzle: [-240.98558, 0.60096154, 30.649038],
        muzzle_is_authored: true,
        clip_size: 6,
        ammo_qty: 50,
        primary: PdFunc {
            label: "Single Shot",
            kind: PdFuncKind::Single,
            flags: 0,
            damage: 2.0,
            spread: 0.0,
            recovery60: 20,
            penetration: 5,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 12.0,
            recoil_angle: 35.0,
            source: "invitems.c:893",
        },
        secondary: Some(PdFunc {
            label: "Pistol Whip",
            kind: PdFuncKind::Melee,
            flags: 4301312,
            damage: 0.9,
            spread: 0.0,
            recovery60: 0,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 60.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:937",
        }),
        view: PdView { muzzlez: 2.0, posx: 9.5, posy: -18.2, posz: -25.5, sway: 1.0 },
        ai: PdAi { score_pri: 68, score_sec: 76, dual_pri: 96, dual_sec: 120, band_pri: 1, band_sec: 0, source: "botinv.c:28" },
        one_handed: true,
        dual_wield: true,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:969",
    },
    // MPWEAPON_DY357LX — The DY357-LX was custom built for NSA director Trent Easton. Besides b
    PdWeapon {
        mp_index: 8,
        name: "DY357-LX",
        fp_model: "guns/dy357trent.bin",
        tp_model: "props/chrdy357trent.bin",
        fp_glb: "pd/08-dy357-lx-fp.glb",
        tp_glb: "pd/08-dy357-lx-tp.glb",
        tp_muzzle: [-240.98558, 0.60096154, 30.649038],
        muzzle_is_authored: true,
        clip_size: 6,
        ammo_qty: 50,
        primary: PdFunc {
            label: "Single Shot",
            kind: PdFuncKind::Single,
            flags: 0,
            damage: 200.0,
            spread: 0.0,
            recovery60: 30,
            penetration: 5,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 12.0,
            recoil_angle: 35.0,
            source: "invitems.c:915",
        },
        secondary: Some(PdFunc {
            label: "Pistol Whip",
            kind: PdFuncKind::Melee,
            flags: 4301312,
            damage: 0.9,
            spread: 0.0,
            recovery60: 0,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 60.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:937",
        }),
        view: PdView { muzzlez: 2.0, posx: 9.5, posy: -18.2, posz: -25.5, sway: 1.0 },
        ai: PdAi { score_pri: 180, score_sec: 188, dual_pri: 184, dual_sec: 188, band_pri: 1, band_sec: 0, source: "botinv.c:29" },
        one_handed: true,
        dual_wield: true,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:994",
    },
    // MPWEAPON_CMP150 — A dataDyne classic, and a bestseller, this submachine gun boasts a 32 
    PdWeapon {
        mp_index: 9,
        name: "CMP150",
        fp_model: "guns/cmp150.bin",
        tp_model: "props/chrcmp150.bin",
        fp_glb: "pd/09-cmp150-fp.glb",
        tp_glb: "pd/09-cmp150-tp.glb",
        tp_muzzle: [-198.91827, -0.60096154, 30.048077],
        muzzle_is_authored: true,
        clip_size: 32,
        ammo_qty: 100,
        primary: PdFunc {
            label: "Rapid Fire",
            kind: PdFuncKind::Auto,
            flags: 0,
            damage: 1.0,
            spread: 9.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 900.0,
            max_rpm: 900.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 4.0,
            recoil_angle: 3.0,
            source: "invitems.c:1357",
        },
        secondary: Some(PdFunc {
            label: "Follow Lock-On",
            kind: PdFuncKind::Auto,
            flags: 0,
            damage: 1.0,
            spread: 9.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 900.0,
            max_rpm: 900.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 4.0,
            recoil_angle: 3.0,
            source: "invitems.c:1385",
        }),
        view: PdView { muzzlez: 3.0, posx: 13.0, posy: -17.7, posz: -27.5, sway: 1.0 },
        ai: PdAi { score_pri: 116, score_sec: 128, dual_pri: 136, dual_sec: 152, band_pri: 2, band_sec: 2, source: "botinv.c:30" },
        one_handed: true,
        dual_wield: true,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:1421",
    },
    // MPWEAPON_CYCLONE — Designed for use by bodyguards, the Cyclone has been adopted by Presid
    PdWeapon {
        mp_index: 10,
        name: "Cyclone",
        fp_model: "guns/cyclone.bin",
        tp_model: "props/chrcyclone.bin",
        fp_glb: "pd/0a-cyclone-fp.glb",
        tp_glb: "pd/0a-cyclone-tp.glb",
        tp_muzzle: [-260.21635, 0.0, -19.230769],
        muzzle_is_authored: true,
        clip_size: 50,
        ammo_qty: 150,
        primary: PdFunc {
            label: "Rapid Fire",
            kind: PdFuncKind::Auto,
            flags: 0,
            damage: 0.8,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 900.0,
            max_rpm: 900.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 5.0,
            recoil_angle: 2.0,
            source: "invitems.c:1485",
        },
        secondary: Some(PdFunc {
            label: "Magazine Discharge",
            kind: PdFuncKind::Auto,
            flags: 32,
            damage: 1.4,
            spread: 25.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 2000.0,
            max_rpm: 2000.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 5.0,
            recoil_angle: 2.0,
            source: "invitems.c:1513",
        }),
        view: PdView { muzzlez: 1.0, posx: 21.5, posy: -26.5, posz: -35.0, sway: 1.0 },
        ai: PdAi { score_pri: 120, score_sec: 128, dual_pri: 132, dual_sec: 140, band_pri: 2, band_sec: 2, source: "botinv.c:31" },
        one_handed: true,
        dual_wield: true,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:1549",
    },
    // MPWEAPON_CALLISTO — Another example of excellent Maian firearm design. It can fire standar
    PdWeapon {
        mp_index: 11,
        name: "Callisto NTG",
        fp_model: "guns/maiansmg.bin",
        tp_model: "props/chrmaiansmg.bin",
        fp_glb: "pd/0b-callisto-ntg-fp.glb",
        tp_glb: "pd/0b-callisto-ntg-tp.glb",
        tp_muzzle: [-456.67085, 9.6298536, 130.30804],
        muzzle_is_authored: false,
        clip_size: 32,
        ammo_qty: 150,
        primary: PdFunc {
            label: "Rapid Fire",
            kind: PdFuncKind::Auto,
            flags: 0,
            damage: 1.2,
            spread: 9.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 900.0,
            max_rpm: 900.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 4.0,
            recoil_angle: 3.0,
            source: "invitems.c:1705",
        },
        secondary: Some(PdFunc {
            label: "High Impact Shells",
            kind: PdFuncKind::Auto,
            flags: 0,
            damage: 2.4,
            spread: 9.0,
            recovery60: 0,
            penetration: 5,
            initial_rpm: 300.0,
            max_rpm: 300.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 4.0,
            recoil_angle: 3.0,
            source: "invitems.c:1733",
        }),
        view: PdView { muzzlez: 3.0, posx: 17.5, posy: -22.7, posz: -25.0, sway: 1.0 },
        ai: PdAi { score_pri: 152, score_sec: 176, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 2, source: "botinv.c:32" },
        one_handed: false,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:1769",
    },
    // MPWEAPON_RCP120 — The Carrington Institute secret weapon. It fires at a phenomenal rate 
    PdWeapon {
        mp_index: 12,
        name: "RC-P120",
        fp_model: "guns/rcp120.bin",
        tp_model: "props/chrrcp120.bin",
        fp_glb: "pd/0c-rc-p120-fp.glb",
        tp_glb: "pd/0c-rc-p120-tp.glb",
        tp_muzzle: [-335.33654, 0.0, 49.278846],
        muzzle_is_authored: true,
        clip_size: 120,
        ammo_qty: 150,
        primary: PdFunc {
            label: "Rapid Fire",
            kind: PdFuncKind::Auto,
            flags: 0,
            damage: 1.2,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 1100.0,
            max_rpm: 1100.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 4.0,
            recoil_angle: 3.0,
            source: "invitems.c:1605",
        },
        secondary: Some(PdFunc {
            label: "Cloak",
            kind: PdFuncKind::Special,
            flags: 1056768,
            damage: 0.0,
            spread: 0.0,
            recovery60: 30,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:1633",
        }),
        view: PdView { muzzlez: 3.0, posx: 13.0, posy: -18.2, posz: -27.5, sway: 1.0 },
        ai: PdAi { score_pri: 172, score_sec: 188, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 2, source: "botinv.c:33" },
        one_handed: false,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:1654",
    },
    // MPWEAPON_LAPTOPGUN — A submachine gun made to look like a laptop PC. In disguised form, the
    PdWeapon {
        mp_index: 13,
        name: "Laptop Gun",
        fp_model: "guns/pcgun.bin",
        tp_model: "props/chrpcgun.bin",
        fp_glb: "pd/0d-laptop-gun-fp.glb",
        tp_glb: "pd/0d-laptop-gun-tp.glb",
        tp_muzzle: [-402.64423, 0.0, 125.0],
        muzzle_is_authored: true,
        clip_size: 50,
        ammo_qty: 150,
        primary: PdFunc {
            label: "Burst Fire",
            kind: PdFuncKind::Auto,
            flags: 2,
            damage: 1.15,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 1000.0,
            max_rpm: 1000.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 5.0,
            recoil_angle: 2.0,
            source: "invitems.c:2390",
        },
        secondary: Some(PdFunc {
            label: "Deploy as Sentry Gun",
            kind: PdFuncKind::Throw,
            flags: 8659264,
            damage: 0.0,
            spread: 0.0,
            recovery60: 60,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:2418",
        }),
        view: PdView { muzzlez: 1.2, posx: 16.0, posy: -17.7, posz: -14.5, sway: 1.0 },
        ai: PdAi { score_pri: 128, score_sec: 140, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 2, source: "botinv.c:34" },
        one_handed: false,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:2440",
    },
    // MPWEAPON_DRAGON — A standard assault rifle with an evil twist - when the secondary mode 
    PdWeapon {
        mp_index: 14,
        name: "Dragon",
        fp_model: "guns/dydragon.bin",
        tp_model: "props/chrdragon.bin",
        fp_glb: "pd/0e-dragon-fp.glb",
        tp_glb: "pd/0e-dragon-tp.glb",
        tp_muzzle: [-558.89423, 0.0, 13.221154],
        muzzle_is_authored: true,
        clip_size: 30,
        ammo_qty: 150,
        primary: PdFunc {
            label: "Rapid Fire",
            kind: PdFuncKind::Auto,
            flags: 0,
            damage: 1.1,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 700.0,
            max_rpm: 700.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 5.0,
            recoil_angle: 2.0,
            source: "invitems.c:1822",
        },
        secondary: Some(PdFunc {
            label: "Proximity Self Destruct",
            kind: PdFuncKind::Throw,
            flags: 270400,
            damage: 0.0,
            spread: 0.0,
            recovery60: 60,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:1850",
        }),
        view: PdView { muzzlez: 1.0, posx: 15.0, posy: -29.5, posz: -27.0, sway: 1.0 },
        ai: PdAi { score_pri: 124, score_sec: 148, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 2, source: "botinv.c:35" },
        one_handed: false,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:1872",
    },
    // MPWEAPON_K7AVENGER — Another piece of high-tech kit from dataDyne. Ordinarily an assault ri
    PdWeapon {
        mp_index: 15,
        name: "K7 Avenger",
        fp_model: "guns/k7avenger.bin",
        tp_model: "props/chravenger.bin",
        fp_glb: "pd/0f-k7-avenger-fp.glb",
        tp_glb: "pd/0f-k7-avenger-tp.glb",
        tp_muzzle: [-488.58173, 1.2019231, 39.663462],
        muzzle_is_authored: true,
        clip_size: 25,
        ammo_qty: 150,
        primary: PdFunc {
            label: "Burst Fire",
            kind: PdFuncKind::Auto,
            flags: 2,
            damage: 1.5,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 950.0,
            max_rpm: 950.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 5.0,
            recoil_angle: 2.0,
            source: "invitems.c:2237",
        },
        secondary: Some(PdFunc {
            label: "Threat Detector",
            kind: PdFuncKind::Auto,
            flags: 532482,
            damage: 1.5,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 950.0,
            max_rpm: 950.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 5.0,
            recoil_angle: 2.0,
            source: "invitems.c:2265",
        }),
        view: PdView { muzzlez: 1.0, posx: 6.5, posy: -24.0, posz: -27.0, sway: 1.0 },
        ai: PdAi { score_pri: 156, score_sec: 180, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 2, source: "botinv.c:36" },
        one_handed: false,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:2301",
    },
    // MPWEAPON_AR34 — The Carrington Institute's main assault rifle. A good range and magazi
    PdWeapon {
        mp_index: 16,
        name: "AR34",
        fp_model: "guns/ar34.bin",
        tp_model: "props/chrar34.bin",
        fp_glb: "pd/10-ar34-fp.glb",
        tp_glb: "pd/10-ar34-tp.glb",
        tp_muzzle: [-498.19712, 0.0, 7.8125],
        muzzle_is_authored: true,
        clip_size: 30,
        ammo_qty: 100,
        primary: PdFunc {
            label: "Burst Fire",
            kind: PdFuncKind::Auto,
            flags: 2,
            damage: 1.4,
            spread: 8.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 750.0,
            max_rpm: 750.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 5.0,
            recoil_angle: 2.0,
            source: "invitems.c:2095",
        },
        secondary: Some(PdFunc {
            label: "Use Scope",
            kind: PdFuncKind::Auto,
            flags: 2,
            damage: 1.4,
            spread: 8.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 750.0,
            max_rpm: 750.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 5.0,
            recoil_angle: 2.0,
            source: "invitems.c:2123",
        }),
        view: PdView { muzzlez: 1.0, posx: 11.5, posy: -25.7, posz: -30.5, sway: 1.0 },
        ai: PdAi { score_pri: 148, score_sec: 176, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 2, source: "botinv.c:37" },
        one_handed: false,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:2159",
    },
    // MPWEAPON_SUPERDRAGON — A variant of the Dragon assault rifle - instead of a proximity explosi
    PdWeapon {
        mp_index: 17,
        name: "SuperDragon",
        fp_model: "guns/dysuperdragon.bin",
        tp_model: "props/chrsuperdragon.bin",
        fp_glb: "pd/11-superdragon-fp.glb",
        tp_glb: "pd/11-superdragon-tp.glb",
        tp_muzzle: [-430.28846, -1.2019231, 18.028846],
        muzzle_is_authored: true,
        clip_size: 30,
        ammo_qty: 150,
        primary: PdFunc {
            label: "Rapid Fire",
            kind: PdFuncKind::Auto,
            flags: 0,
            damage: 1.2,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 700.0,
            max_rpm: 700.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 5.0,
            recoil_angle: 2.0,
            source: "invitems.c:1956",
        },
        secondary: Some(PdFunc {
            label: "Grenade Launcher",
            kind: PdFuncKind::Projectile,
            flags: 805306432,
            damage: 1.2,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 1200,
            melee_range: 0.0,
            recoil_dist: 3.0,
            recoil_angle: 2.0,
            source: "invitems.c:1984",
        }),
        view: PdView { muzzlez: 1.0, posx: 15.0, posy: -29.5, posz: -27.0, sway: 1.0 },
        ai: PdAi { score_pri: 164, score_sec: 188, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 3, source: "botinv.c:38" },
        one_handed: false,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:2031",
    },
    // MPWEAPON_SHOTGUN — A dataDyne weapon manufactured for security forces. A nine-cartridge m
    PdWeapon {
        mp_index: 18,
        name: "Shotgun",
        fp_model: "guns/shotgun.bin",
        tp_model: "props/chrshotgun.bin",
        fp_glb: "pd/12-shotgun-fp.glb",
        tp_glb: "pd/12-shotgun-tp.glb",
        tp_muzzle: [-587.74038, -3.6057692, 67.307692],
        muzzle_is_authored: true,
        clip_size: 9,
        ammo_qty: 16,
        primary: PdFunc {
            label: "Shotgun Fire",
            kind: PdFuncKind::Single,
            flags: 0,
            damage: 0.6,
            spread: 30.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:2505",
        },
        secondary: Some(PdFunc {
            label: "Double Blast",
            kind: PdFuncKind::Single,
            flags: 4096,
            damage: 0.6,
            spread: 16.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:2527",
        }),
        view: PdView { muzzlez: 1.0, posx: 12.0, posy: -16.7, posz: -21.0, sway: 1.0 },
        ai: PdAi { score_pri: 140, score_sec: 156, dual_pri: 0, dual_sec: 0, band_pri: 1, band_sec: 1, source: "botinv.c:39" },
        one_handed: false,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:2557",
    },
    // MPWEAPON_REAPER — A truly terrifying weapon in the hands of someone strong enough to con
    PdWeapon {
        mp_index: 19,
        name: "Reaper",
        fp_model: "guns/skminigun.bin",
        tp_model: "props/chrskminigun.bin",
        fp_glb: "pd/13-reaper-fp.glb",
        tp_glb: "pd/13-reaper-tp.glb",
        tp_muzzle: [-444.11058, -312.5, -80.528846],
        muzzle_is_authored: true,
        clip_size: 200,
        ammo_qty: 200,
        primary: PdFunc {
            label: "Reapage",
            kind: PdFuncKind::Auto,
            flags: 2,
            damage: 1.2,
            spread: 56.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 60.0,
            max_rpm: 1800.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:2630",
        },
        secondary: Some(PdFunc {
            label: "Grinder",
            kind: PdFuncKind::Melee,
            flags: 8192,
            damage: 0.05,
            spread: 0.0,
            recovery60: 0,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 80.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:2658",
        }),
        view: PdView { muzzlez: 1.0, posx: 4.0, posy: -21.2, posz: -30.5, sway: 1.0 },
        ai: PdAi { score_pri: 144, score_sec: 176, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 0, source: "botinv.c:40" },
        one_handed: false,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:2690",
    },
    // MPWEAPON_SNIPERRIFLE — With a powerful zoom and a high velocity bullet, this Carrington Insti
    PdWeapon {
        mp_index: 20,
        name: "Sniper Rifle",
        fp_model: "guns/sniperrifle.bin",
        tp_model: "props/chrsniperrifle.bin",
        fp_glb: "pd/14-sniper-rifle-fp.glb",
        tp_glb: "pd/14-sniper-rifle-tp.glb",
        tp_muzzle: [-942.3748, 9.6299029, 145.29024],
        muzzle_is_authored: false,
        clip_size: 8,
        ammo_qty: 50,
        primary: PdFunc {
            label: "Single Shot",
            kind: PdFuncKind::Single,
            flags: 8192,
            damage: 1.2,
            spread: 0.0,
            recovery60: 16,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 8.0,
            recoil_angle: 0.0,
            source: "invitems.c:4023",
        },
        secondary: Some(PdFunc {
            label: "Crouch",
            kind: PdFuncKind::Special,
            flags: 1056768,
            damage: 0.0,
            spread: 0.0,
            recovery60: 30,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:4045",
        }),
        view: PdView { muzzlez: 6.0, posx: 21.0, posy: -27.2, posz: -31.5, sway: 1.0 },
        ai: PdAi { score_pri: 28, score_sec: 40, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 2, source: "botinv.c:41" },
        one_handed: false,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:4071",
    },
    // MPWEAPON_FARSIGHT — The FarSight rifle is a Maian hybrid of an X-ray scanning device coupl
    PdWeapon {
        mp_index: 21,
        name: "FarSight XR-20",
        fp_model: "guns/z2020.bin",
        tp_model: "props/chrz2020.bin",
        fp_glb: "pd/15-farsight-xr-20-fp.glb",
        tp_glb: "pd/15-farsight-xr-20-tp.glb",
        tp_muzzle: [-1032.7452, 7.2953416, 123.5351],
        muzzle_is_authored: false,
        clip_size: 8,
        ammo_qty: 10,
        primary: PdFunc {
            label: "Rail-gun effect",
            kind: PdFuncKind::Single,
            flags: 0,
            damage: 100.0,
            spread: 0.0,
            recovery60: 0,
            penetration: 5,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:3573",
        },
        secondary: Some(PdFunc {
            label: "Target Locator",
            kind: PdFuncKind::Single,
            flags: 0,
            damage: 100.0,
            spread: 0.0,
            recovery60: 0,
            penetration: 5,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:3595",
        }),
        view: PdView { muzzlez: 6.0, posx: 21.5, posy: -25.2, posz: -32.5, sway: 1.0 },
        ai: PdAi { score_pri: 188, score_sec: 188, dual_pri: 0, dual_sec: 0, band_pri: 3, band_sec: 5, source: "botinv.c:42" },
        one_handed: false,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:3630",
    },
    // MPWEAPON_DEVASTATOR — A long range grenade delivery system manufactured by dataDyne. The sec
    PdWeapon {
        mp_index: 22,
        name: "Devastator",
        fp_model: "guns/dydevastator.bin",
        tp_model: "props/chrdevastator.bin",
        fp_glb: "pd/16-devastator-fp.glb",
        tp_glb: "pd/16-devastator-tp.glb",
        tp_muzzle: [-601.88869, -28.809236, 121.75315],
        muzzle_is_authored: false,
        clip_size: 8,
        ammo_qty: 16,
        primary: PdFunc {
            label: "Grenade Launcher",
            kind: PdFuncKind::Projectile,
            flags: 805306432,
            damage: 1.0,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 1200,
            melee_range: 0.0,
            recoil_dist: 5.0,
            recoil_angle: 8.0,
            source: "invitems.c:2988",
        },
        secondary: Some(PdFunc {
            label: "Wall Hugger",
            kind: PdFuncKind::Projectile,
            flags: 805306688,
            damage: 1.0,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 360,
            melee_range: 0.0,
            recoil_dist: 5.0,
            recoil_angle: 8.0,
            source: "invitems.c:3019",
        }),
        view: PdView { muzzlez: 1.0, posx: 19.5, posy: -25.5, posz: -29.0, sway: 1.0 },
        ai: PdAi { score_pri: 176, score_sec: 188, dual_pri: 0, dual_sec: 0, band_pri: 3, band_sec: 3, source: "botinv.c:43" },
        one_handed: false,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:3063",
    },
    // MPWEAPON_ROCKETLAUNCHER — A cumbersome weapon. Fires either a standard rocket or a slower, homin
    PdWeapon {
        mp_index: 23,
        name: "Rocket Launcher",
        fp_model: "guns/dyrocket.bin",
        tp_model: "props/chrdyrocket.bin",
        fp_glb: "pd/17-rocket-launcher-fp.glb",
        tp_glb: "pd/17-rocket-launcher-tp.glb",
        tp_muzzle: [-470.61327, -16.984915, 185.50009],
        muzzle_is_authored: false,
        clip_size: 1,
        ammo_qty: 3,
        primary: PdFunc {
            label: "Rocket Launch",
            kind: PdFuncKind::Projectile,
            flags: 134217792,
            damage: 1.0,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 60.0,
            projectile_timer60: -1,
            melee_range: 0.0,
            recoil_dist: 3.0,
            recoil_angle: 2.0,
            source: "invitems.c:2758",
        },
        secondary: Some(PdFunc {
            label: "Targeted Rocket",
            kind: PdFuncKind::Projectile,
            flags: 1207959616,
            damage: 1.0,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: -1,
            melee_range: 0.0,
            recoil_dist: 3.0,
            recoil_angle: 2.0,
            source: "invitems.c:2789",
        }),
        view: PdView { muzzlez: 1.0, posx: 24.5, posy: -25.2, posz: -30.0, sway: 1.0 },
        ai: PdAi { score_pri: 160, score_sec: 188, dual_pri: 0, dual_sec: 0, band_pri: 3, band_sec: 3, source: "botinv.c:44" },
        one_handed: false,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:2828",
    },
    // MPWEAPON_SLAYER — The Skedar enjoy seeing the terror of their enemies. It seems natural 
    PdWeapon {
        mp_index: 24,
        name: "Slayer",
        fp_model: "guns/skrocket.bin",
        tp_model: "props/chrskrocket.bin",
        fp_glb: "pd/18-slayer-fp.glb",
        tp_glb: "pd/18-slayer-tp.glb",
        tp_muzzle: [-566.1934, -21.933342, 256.75464],
        muzzle_is_authored: false,
        clip_size: 1,
        ammo_qty: 3,
        primary: PdFunc {
            label: "Rocket Launch",
            kind: PdFuncKind::Projectile,
            flags: 134217792,
            damage: 1.0,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 10.0,
            projectile_timer60: -1,
            melee_range: 0.0,
            recoil_dist: 3.0,
            recoil_angle: 2.0,
            source: "invitems.c:2868",
        },
        secondary: Some(PdFunc {
            label: "Fly-By-Wire Rocket",
            kind: PdFuncKind::Projectile,
            flags: 671090752,
            damage: 1.0,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 10.0,
            projectile_timer60: -1,
            melee_range: 0.0,
            recoil_dist: 3.0,
            recoil_angle: 2.0,
            source: "invitems.c:2899",
        }),
        view: PdView { muzzlez: 1.0, posx: 22.5, posy: -32.0, posz: -40.5, sway: 1.0 },
        ai: PdAi { score_pri: 168, score_sec: 188, dual_pri: 0, dual_sec: 0, band_pri: 3, band_sec: 3, source: "botinv.c:45" },
        one_handed: false,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:2938",
    },
    // MPWEAPON_COMBATKNIFE — A large and vicious combat knife. It contains a vial of poison that sh
    PdWeapon {
        mp_index: 25,
        name: "Combat Knife",
        fp_model: "guns/knife.bin",
        tp_model: "props/chrknife.bin",
        fp_glb: "pd/19-combat-knife-fp.glb",
        tp_glb: "pd/19-combat-knife-tp.glb",
        tp_muzzle: [-137.25212, -6.7497165, 166.59489],
        muzzle_is_authored: false,
        clip_size: 1,
        ammo_qty: 5,
        primary: PdFunc {
            label: "Knife Slash",
            kind: PdFuncKind::Melee,
            flags: 8192,
            damage: 2.0,
            spread: 0.0,
            recovery60: 0,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 70.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:4901",
        },
        secondary: Some(PdFunc {
            label: "Throw Poison Knife",
            kind: PdFuncKind::Throw,
            flags: 8396800,
            damage: 1.0,
            spread: 0.0,
            recovery60: 60,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:4925",
        }),
        view: PdView { muzzlez: 1.0, posx: 18.5, posy: -26.5, posz: -28.0, sway: 1.0 },
        ai: PdAi { score_pri: 20, score_sec: 40, dual_pri: 24, dual_sec: 40, band_pri: 0, band_sec: 2, source: "botinv.c:46" },
        one_handed: true,
        dual_wield: true,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:4947",
    },
    // MPWEAPON_CROSSBOW — This crossbow is a short-range 'pistol' sized example, mounted on a Ca
    PdWeapon {
        mp_index: 26,
        name: "Crossbow",
        fp_model: "guns/crossbow.bin",
        tp_model: "props/chrcrossbow.bin",
        fp_glb: "pd/1a-crossbow-fp.glb",
        tp_glb: "pd/1a-crossbow-tp.glb",
        tp_muzzle: [4.0174356, 18.24336, 56.493576],
        muzzle_is_authored: false,
        clip_size: 5,
        ammo_qty: 10,
        primary: PdFunc {
            label: "Sedate",
            kind: PdFuncKind::Projectile,
            flags: 8397312,
            damage: 1.0,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: -1,
            melee_range: 0.0,
            recoil_dist: 3.0,
            recoil_angle: 2.0,
            source: "invitems.c:3728",
        },
        secondary: Some(PdFunc {
            label: "Instant Kill",
            kind: PdFuncKind::Projectile,
            flags: 8396800,
            damage: 100.0,
            spread: 6.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: -1,
            melee_range: 0.0,
            recoil_dist: 3.0,
            recoil_angle: 2.0,
            source: "invitems.c:3697",
        }),
        view: PdView { muzzlez: 1.0, posx: 11.0, posy: -15.0, posz: -21.0, sway: 1.0 },
        ai: PdAi { score_pri: 108, score_sec: 176, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 2, source: "botinv.c:47" },
        one_handed: true,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:3774",
    },
    // MPWEAPON_TRANQUILIZER — A rapid-fire device, it can be used as a weapon in an emergency, but i
    PdWeapon {
        mp_index: 27,
        name: "Tranquilizer",
        fp_model: "guns/druggun.bin",
        tp_model: "props/chrdruggun.bin",
        fp_glb: "pd/1b-tranquilizer-fp.glb",
        tp_glb: "pd/1b-tranquilizer-tp.glb",
        tp_muzzle: [-190.3588, -21.3842, 71.804404],
        muzzle_is_authored: false,
        clip_size: 8,
        ammo_qty: 50,
        primary: PdFunc {
            label: "Sedate",
            kind: PdFuncKind::Single,
            flags: 512,
            damage: 0.25,
            spread: 3.0,
            recovery60: 16,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 1.0,
            recoil_angle: 0.0,
            source: "invitems.c:3838",
        },
        secondary: Some(PdFunc {
            label: "Lethal Injection",
            kind: PdFuncKind::Melee,
            flags: 8192,
            damage: 100.0,
            spread: 0.0,
            recovery60: 0,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 60.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:3860",
        }),
        view: PdView { muzzlez: 1.0, posx: 10.0, posy: -15.2, posz: -24.0, sway: 1.0 },
        ai: PdAi { score_pri: 48, score_sec: 188, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 0, source: "botinv.c:48" },
        one_handed: true,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:3899",
    },
    // MPWEAPON_GRENADE — An updated version of the trusty grenade. Can be thrown with a four-se
    PdWeapon {
        mp_index: 28,
        name: "Grenade",
        fp_model: "guns/grenade.bin",
        tp_model: "props/chrgrenade.bin",
        fp_glb: "pd/1c-grenade-fp.glb",
        tp_glb: "pd/1c-grenade-tp.glb",
        tp_muzzle: [-175.6165, 11.250436, 70.983717],
        muzzle_is_authored: false,
        clip_size: 1,
        ammo_qty: 5,
        primary: PdFunc {
            label: "4-Second Fuse",
            kind: PdFuncKind::Throw,
            flags: 8256,
            damage: 0.0,
            spread: 0.0,
            recovery60: 60,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:3420",
        },
        secondary: Some(PdFunc {
            label: "Proximity Pinball",
            kind: PdFuncKind::Throw,
            flags: 8256,
            damage: 0.0,
            spread: 0.0,
            recovery60: 60,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:3434",
        }),
        view: PdView { muzzlez: 1.0, posx: 17.0, posy: -19.7, posz: -21.0, sway: 1.0 },
        ai: PdAi { score_pri: 36, score_sec: 172, dual_pri: 0, dual_sec: 0, band_pri: 7, band_sec: 7, source: "botinv.c:50" },
        one_handed: true,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:3456",
    },
    // MPWEAPON_NBOMB — A hand-held, small area effect neutron bomb. It can either detonate on
    PdWeapon {
        mp_index: 29,
        name: "N-Bomb",
        fp_model: "guns/nbomb.bin",
        tp_model: "props/chrnbomb.bin",
        fp_glb: "pd/1d-n-bomb-fp.glb",
        tp_glb: "pd/1d-n-bomb-tp.glb",
        tp_muzzle: [-175.6165, 11.250436, 70.983717],
        muzzle_is_authored: false,
        clip_size: 1,
        ammo_qty: 3,
        primary: PdFunc {
            label: "Impact Detonation",
            kind: PdFuncKind::Throw,
            flags: 9792,
            damage: 0.0,
            spread: 0.0,
            recovery60: 60,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:3481",
        },
        secondary: Some(PdFunc {
            label: "Proximity Detonation",
            kind: PdFuncKind::Throw,
            flags: 9792,
            damage: 0.0,
            spread: 0.0,
            recovery60: 60,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:3495",
        }),
        view: PdView { muzzlez: 1.0, posx: 17.0, posy: -19.7, posz: -21.0, sway: 1.0 },
        ai: PdAi { score_pri: 32, score_sec: 188, dual_pri: 0, dual_sec: 0, band_pri: 7, band_sec: 7, source: "botinv.c:51" },
        one_handed: true,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:3517",
    },
    // MPWEAPON_TIMEDMINE — A mine with a short timed fuse. It has a threat detection/evaluation s
    PdWeapon {
        mp_index: 30,
        name: "Timed Mine",
        fp_model: "guns/timedmine.bin",
        tp_model: "props/chrtimedmine.bin",
        fp_glb: "pd/1e-timed-mine-fp.glb",
        tp_glb: "pd/1e-timed-mine-tp.glb",
        tp_muzzle: [-202.71708, -8.9696262, -0.67542429],
        muzzle_is_authored: false,
        clip_size: 1,
        ammo_qty: 5,
        primary: PdFunc {
            label: "Timed Explosive",
            kind: PdFuncKind::Throw,
            flags: 8396864,
            damage: 0.0,
            spread: 0.0,
            recovery60: 0,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:3115",
        },
        secondary: Some(PdFunc {
            label: "Threat Detector",
            kind: PdFuncKind::Special,
            flags: 524288,
            damage: 0.0,
            spread: 0.0,
            recovery60: 0,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:3088",
        }),
        view: PdView { muzzlez: 1.0, posx: 8.0, posy: -15.0, posz: -23.0, sway: 1.0 },
        ai: PdAi { score_pri: 12, score_sec: 12, dual_pri: 0, dual_sec: 0, band_pri: 7, band_sec: 2, source: "botinv.c:52" },
        one_handed: true,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:3137",
    },
    // MPWEAPON_PROXIMITYMINE — A mine with a proximity fuse. It has a threat detection/evaluation sen
    PdWeapon {
        mp_index: 31,
        name: "Proximity Mine",
        fp_model: "guns/proximitymine.bin",
        tp_model: "props/chrproximitymine.bin",
        fp_glb: "pd/1f-proximity-mine-fp.glb",
        tp_glb: "pd/1f-proximity-mine-tp.glb",
        tp_muzzle: [-202.71708, -8.9696262, -0.67542429],
        muzzle_is_authored: false,
        clip_size: 1,
        ammo_qty: 5,
        primary: PdFunc {
            label: "Proximity Explosive",
            kind: PdFuncKind::Throw,
            flags: 8396864,
            damage: 0.0,
            spread: 0.0,
            recovery60: 0,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:3260",
        },
        secondary: Some(PdFunc {
            label: "Threat Detector",
            kind: PdFuncKind::Special,
            flags: 524288,
            damage: 0.0,
            spread: 0.0,
            recovery60: 0,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:3088",
        }),
        view: PdView { muzzlez: 1.0, posx: 8.0, posy: -15.0, posz: -23.0, sway: 1.0 },
        ai: PdAi { score_pri: 40, score_sec: 176, dual_pri: 0, dual_sec: 0, band_pri: 7, band_sec: 2, source: "botinv.c:53" },
        one_handed: true,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:3282",
    },
    // MPWEAPON_REMOTEMINE — A mine that can be triggered remotely. The activate command is the sec
    PdWeapon {
        mp_index: 32,
        name: "Remote Mine",
        fp_model: "guns/remotemine.bin",
        tp_model: "props/chrremotemine.bin",
        fp_glb: "pd/20-remote-mine-fp.glb",
        tp_glb: "pd/20-remote-mine-tp.glb",
        tp_muzzle: [-202.71708, -8.9696262, -0.67542429],
        muzzle_is_authored: false,
        clip_size: 1,
        ammo_qty: 5,
        primary: PdFunc {
            label: "Remote Explosive",
            kind: PdFuncKind::Throw,
            flags: 8396864,
            damage: 0.0,
            spread: 0.0,
            recovery60: 0,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:3191",
        },
        secondary: Some(PdFunc {
            label: "Detonate",
            kind: PdFuncKind::Special,
            flags: 1056768,
            damage: 0.0,
            spread: 0.0,
            recovery60: 30,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:3205",
        }),
        view: PdView { muzzlez: 1.0, posx: 4.0, posy: -15.0, posz: -23.0, sway: 1.0 },
        ai: PdAi { score_pri: 44, score_sec: 156, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 2, source: "botinv.c:54" },
        one_handed: true,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:3231",
    },
    // MPWEAPON_LASER — The laser is wrist-mounted and deadly accurate. It can either fire lon
    PdWeapon {
        mp_index: 33,
        name: "Laser",
        fp_model: "guns/laser.bin",
        tp_model: "props/chrlaser.bin",
        fp_glb: "pd/21-laser-fp.glb",
        tp_glb: "pd/21-laser-tp.glb",
        tp_muzzle: [115.66738, -0.2685649, 57.99465],
        muzzle_is_authored: false,
        clip_size: 0,
        ammo_qty: 0,
        primary: PdFunc {
            label: "Pulse Fire",
            kind: PdFuncKind::Single,
            flags: 0,
            damage: 1.0,
            spread: 0.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:4110",
        },
        secondary: Some(PdFunc {
            label: "Short Range Stream",
            kind: PdFuncKind::Auto,
            flags: 0,
            damage: 0.1,
            spread: 0.0,
            recovery60: 0,
            penetration: 1,
            initial_rpm: 3600.0,
            max_rpm: 3600.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 4.0,
            recoil_angle: 3.0,
            source: "invitems.c:4132",
        }),
        view: PdView { muzzlez: 3.0, posx: -12.0, posy: -12.7, posz: -21.5, sway: 1.0 },
        ai: PdAi { score_pri: 112, score_sec: 112, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 0, source: "botinv.c:49" },
        one_handed: true,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: false,
        source: "invitems.c:4160",
    },
    // MPWEAPON_XRAYSCANNER — A short-range scope that can see through any material - even lead - pr
    PdWeapon {
        mp_index: 34,
        name: "X-Ray Scanner",
        fp_model: "props/xrayspecs.bin",
        tp_model: "props/chrnightsight.bin",
        fp_glb: "pd/22-x-ray-scanner-fp.glb",
        tp_glb: "pd/22-x-ray-scanner-tp.glb",
        tp_muzzle: [0.0, 0.0, 0.0],
        muzzle_is_authored: false,
        clip_size: 0,
        ammo_qty: 0,
        primary: PdFunc {
            label: "X-Ray Vision",
            kind: PdFuncKind::Device,
            flags: 8192,
            damage: 0.0,
            spread: 0.0,
            recovery60: 0,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:5489",
        },
        secondary: None,
        view: PdView { muzzlez: 1.0, posx: 0.0, posy: -39.5, posz: -55.5, sway: 1.0 },
        ai: PdAi { score_pri: 4, score_sec: 4, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 2, source: "botinv.c:67" },
        one_handed: true,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: true,
        source: "invitems.c:5500",
    },
    // MPWEAPON_CLOAKINGDEVICE — Uses the light-warping qualities of an alien crystal to create a refra
    PdWeapon {
        mp_index: 35,
        name: "Cloaking Device",
        fp_model: "props/chrcloaker.bin",
        tp_model: "props/chrcloaker.bin",
        fp_glb: "pd/23-cloaking-device-fp.glb",
        tp_glb: "pd/23-cloaking-device-tp.glb",
        tp_muzzle: [0.0, 0.0, 0.0],
        muzzle_is_authored: false,
        clip_size: 10,
        ammo_qty: 0,
        primary: PdFunc {
            label: "Cloak",
            kind: PdFuncKind::Device,
            flags: 8192,
            damage: 0.0,
            spread: 0.0,
            recovery60: 0,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:5170",
        },
        secondary: None,
        view: PdView { muzzlez: 1.0, posx: 0.0, posy: -39.5, posz: -55.5, sway: 1.0 },
        ai: PdAi { score_pri: 218, score_sec: 218, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 2, source: "botinv.c:69" },
        one_handed: true,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: true,
        source: "invitems.c:5189",
    },
    // MPWEAPON_COMBATBOOST — Geneered stimulants designed for combat applications. When administere
    PdWeapon {
        mp_index: 36,
        name: "Combat Boost",
        fp_model: "props/chrspeedpill.bin",
        tp_model: "props/chrspeedpill.bin",
        fp_glb: "pd/24-combat-boost-fp.glb",
        tp_glb: "pd/24-combat-boost-tp.glb",
        tp_muzzle: [0.0, 0.0, 0.0],
        muzzle_is_authored: false,
        clip_size: 4,
        ammo_qty: 0,
        primary: PdFunc {
            label: "Boost",
            kind: PdFuncKind::Special,
            flags: 8192,
            damage: 0.0,
            spread: 0.0,
            recovery60: 30,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:5214",
        },
        secondary: Some(PdFunc {
            label: "Revert",
            kind: PdFuncKind::Special,
            flags: 8192,
            damage: 0.0,
            spread: 0.0,
            recovery60: 30,
            penetration: 0,
            initial_rpm: 0.0,
            max_rpm: 0.0,
            projectile_speed: 0.0,
            projectile_timer60: 0,
            melee_range: 0.0,
            recoil_dist: 0.0,
            recoil_angle: 0.0,
            source: "invitems.c:5227",
        }),
        view: PdView { muzzlez: 1.0, posx: 0.0, posy: -39.5, posz: -55.5, sway: 1.0 },
        ai: PdAi { score_pri: 8, score_sec: 8, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 2, source: "botinv.c:55" },
        one_handed: true,
        dual_wield: false,
        ai_can_use: true,
        equipment_only: true,
        source: "invitems.c:5248",
    },
    // MPWEAPON_SHIELD — 
    PdWeapon {
        mp_index: 37,
        name: "",
        fp_model: "",
        tp_model: "props/chrshield.bin",
        fp_glb: "pd/25--fp.glb",
        tp_glb: "pd/25--tp.glb",
        tp_muzzle: [0.0, 0.0, 0.0],
        muzzle_is_authored: false,
        clip_size: 30,
        ammo_qty: 0,
        primary: PdFunc::INERT,
        secondary: None,
        view: PdView { muzzlez: 1.0, posx: 12.5, posy: -17.0, posz: -27.5, sway: 1.0 },
        ai: PdAi { score_pri: 220, score_sec: 220, dual_pri: 0, dual_sec: 0, band_pri: 2, band_sec: 2, source: "botinv.c:111" },
        one_handed: true,
        dual_wield: true,
        ai_can_use: true,
        equipment_only: true,
        source: "invitems.c:179",
    },
];


/// The guns — the MP set minus the four equipment entries. This is the port's
/// scope (user decision; see the `pd-arsenal-decisions` note).
pub fn pd_guns() -> impl Iterator<Item = &'static PdWeapon> {
    PD_WEAPONS.iter().filter(|w| !w.equipment_only)
}

/// Look a weapon up by its `MPWEAPON_*` index.
pub fn pd_weapon(mp_index: u8) -> Option<&'static PdWeapon> {
    PD_WEAPONS.iter().find(|w| w.mp_index == mp_index)
}

/// Look a weapon up by its authored name.
pub fn pd_weapon_by_name(name: &str) -> Option<&'static PdWeapon> {
    PD_WEAPONS.iter().find(|w| w.name == name)
}

/// An explosion type by its `EXPLOSIONTYPE_*` index.
pub fn pd_explosion(index: u8) -> Option<&'static PdExplosion> {
    PD_EXPLOSIONS.iter().find(|e| e.index == index)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scope decision, pinned: 33 guns out of the 37 MP entries, with the
    /// four equipment ones separated rather than silently dropped.
    #[test]
    fn the_mp_set_is_33_guns_plus_4_equipment() {
        assert_eq!(pd_guns().count(), 33, "the MP gun set");
        assert_eq!(
            PD_WEAPONS.iter().filter(|w| w.equipment_only).count(),
            4,
            "X-Ray, Cloak, Combat Boost, Shield"
        );
    }

    /// PD's defining feature: every gun has a real second function. If this ever
    /// fails, the generator lost a `functions[2]` column.
    #[test]
    fn every_gun_has_two_functions() {
        for w in pd_guns() {
            assert!(w.secondary.is_some(), "{} has no secondary function", w.name);
        }
    }

    /// Both models resolve for every gun, and they are the two DIFFERENT models
    /// PD ships per weapon — the first-person one and the `chr*` one that carries
    /// `CHRGUNFIRE`. Grabbing the same file for both is the documented easy
    /// mistake (`DESIGN_PD_WEAPON_MECHANICS.md` §3).
    #[test]
    fn every_gun_has_both_models() {
        for w in pd_guns() {
            assert!(!w.fp_model.is_empty(), "{} has no first-person model", w.name);
            assert!(!w.tp_model.is_empty(), "{} has no third-person model", w.name);
            assert!(
                w.fp_model.starts_with("guns/"),
                "{} first-person model is not from guns/: {}",
                w.name,
                w.fp_model
            );
            assert!(
                w.tp_model.starts_with("props/chr"),
                "{} third-person model is not a props/chr* file: {}",
                w.name,
                w.tp_model
            );
            assert_ne!(w.fp_model, w.tp_model, "{} uses one model for both", w.name);
        }
    }

    /// Spot-check the Falcon 2 against the source by hand. If the generator ever
    /// slips a column, a row that reads plausibly is the failure mode — so one
    /// row is checked field by field against `invitems.c:485` and `:568`.
    #[test]
    fn falcon2_matches_the_source() {
        let w = pd_weapon_by_name("Falcon 2").expect("Falcon 2 in the table");
        assert_eq!(w.mp_index, 0x01);
        assert_eq!(w.fp_model, "guns/falcon2.bin");
        assert_eq!(w.tp_model, "props/chrfalcon2.bin");
        assert_eq!(w.clip_size, 8, "invammo_falcon2 clip size");
        assert!(w.one_handed && w.dual_wield);

        let p = &w.primary;
        assert_eq!(p.kind, PdFuncKind::Single);
        assert_eq!(p.damage, 1.0);
        assert_eq!(p.spread, 1.0);
        assert_eq!(p.recovery60, 16);
        assert_eq!(p.penetration, 1);

        // The secondary is the pistol whip — a melee function on a pistol, which
        // is exactly the sort of thing one fire mode per weapon could not express.
        let s = w.secondary.as_ref().expect("pistol whip");
        assert_eq!(s.kind, PdFuncKind::Melee);
        assert_eq!(s.damage, 0.9);
        assert!(s.has_flag(FUNCFLAG_MAKEDIZZY), "the whip knocks out");
        assert!(s.has_flag(FUNCFLAG_NOMUZZLEFLASH), "no flash on a melee");
    }

    /// The damage conversion lands where the derivation says it should: four
    /// Falcon 2 body shots kill a 100 HP hunter, matching PD's `maxdamage = 4`.
    #[test]
    fn the_damage_scale_gives_four_shots_to_kill() {
        let w = pd_weapon_by_name("Falcon 2").unwrap();
        let shots = (100.0 / w.primary.damage_hp()).ceil();
        assert_eq!(shots, 4.0, "PD's guard takes 4 Falcon 2 rounds; so must ours");
    }

    /// Automatics carry a real spin-up, and it is a spin-UP.
    #[test]
    fn automatics_spin_up() {
        let autos: Vec<_> = pd_guns()
            .filter(|w| w.primary.kind == PdFuncKind::Auto)
            .collect();
        assert!(autos.len() >= 8, "PD has plenty of automatics, got {}", autos.len());
        for w in &autos {
            let f = &w.primary;
            assert!(f.max_rpm > 0.0, "{} has no max rpm", w.name);
            assert!(
                f.initial_rpm <= f.max_rpm,
                "{} winds DOWN: {} -> {}",
                w.name,
                f.initial_rpm,
                f.max_rpm
            );
            assert!(f.sustained_cooldown() > 0.0, "{} has no cadence", w.name);
        }
    }

    /// The engagement bands order the way the weapons do: a knife wants contact,
    /// a FarSight wants the far side of the room. This is the authored answer to
    /// "standoff should scale with weapon range".
    #[test]
    fn engagement_bands_scale_with_the_weapon() {
        let knife = pd_weapon_by_name("Combat Knife").unwrap();
        let pistol = pd_weapon_by_name("Falcon 2").unwrap();
        let rocket = pd_weapon_by_name("Rocket Launcher").unwrap();
        let farsight = pd_weapon_by_name("FarSight XR-20").unwrap();

        assert!(knife.band_m(false).1 <= 1.5, "a knife closes to contact");
        assert!(pistol.band_m(false).0 >= 2.0, "a pistol holds off a little");
        assert!(
            rocket.band_m(false).0 > pistol.band_m(false).1,
            "a rocket stands off further than a pistol"
        );
        assert!(farsight.band_m(true).1 >= 20.0, "the FarSight reaches right across");

        // Every band is a real interval.
        for b in PD_DIST_BANDS.iter() {
            assert!(b.max_m > b.min_m, "{} is not an interval", b.name);
        }
    }

    /// The structural point about PD explosions: the lethal volume is bigger than
    /// the fireball, and blasts have a duration to grow across. If a port ever
    /// collapses these back to one sphere, this fails.
    #[test]
    fn explosions_separate_blast_from_damage_radius() {
        let lethal: Vec<_> = PD_EXPLOSIONS.iter().filter(|e| e.damage > 0.0).collect();
        assert!(lethal.len() >= 20, "most explosion types do damage");
        assert!(
            lethal.iter().any(|e| e.damage_radius_m > e.blast_radius_m * 1.5),
            "some blast reaches well past its fireball"
        );
        for e in &lethal {
            assert!(
                e.damage_radius_m >= e.blast_radius_m,
                "{} damages inside its own fireball only",
                e.name
            );
            assert!(e.duration_s > 0.0, "{} has no duration to propagate over", e.name);
        }
    }

    /// The rocket explosion is recognisably the one we authored by hand, which is
    /// the reassurance that adopting the table will not upend the feel: PD's
    /// damage radius is 4 m against our authored 5 m.
    #[test]
    fn the_rocket_explosion_is_close_to_our_authored_one() {
        let rocket = PD_EXPLOSIONS
            .iter()
            .find(|e| e.name == "EXPLOSIONTYPE_ROCKET")
            .expect("a named rocket explosion");
        assert!(
            (rocket.damage_radius_m - 4.0).abs() < 0.01,
            "PD's rocket damage radius, got {}",
            rocket.damage_radius_m
        );
        assert!(rocket.peak_damage_hp() > 100.0, "a direct rocket hit is lethal");
    }

    /// Every gun's two exported GLBs exist on disk, named as
    /// `pd_gltf.py guns` writes them. This is the seam where the weapon table and
    /// the asset export can silently drift apart — a row naming a model nobody
    /// exported reads fine in code and draws nothing in game.
    #[test]
    fn every_gun_has_its_exported_glbs() {
        let dir = format!("{}/../../assets/weapons/pd", env!("CARGO_MANIFEST_DIR"));
        if !std::path::Path::new(&dir).is_dir() {
            // The export is reproducible from the (gitignored) decomp, so a clone
            // without it should not fail the suite — but say so rather than pass
            // quietly, since a silent skip is how this stops testing anything.
            eprintln!("note: {dir} absent — run `pd_gltf.py guns` to check the assets");
            return;
        }
        for w in pd_guns() {
            let slug: String = w
                .name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
                .collect();
            let mut slug = slug;
            while slug.contains("--") {
                slug = slug.replace("--", "-");
            }
            let slug = slug.trim_matches('-');
            for role in ["fp", "tp"] {
                let path = format!("{dir}/{:02x}-{slug}-{role}.glb", w.mp_index);
                assert!(
                    std::path::Path::new(&path).is_file(),
                    "{} is missing its {role} model at {path}",
                    w.name
                );
            }
        }
    }

    /// Provenance is not decoration — every row must be traceable, because the
    /// whole reason this file is generated is so a wrong number can be found.
    #[test]
    fn every_row_cites_its_source() {
        for w in PD_WEAPONS.iter() {
            assert!(w.source.starts_with("invitems.c:"), "{} has no source", w.name);
        }
        // Functions are checked on the guns only: `PdFunc::INERT` deliberately has
        // no source, and exactly one equipment row uses it.
        for w in pd_guns() {
            assert!(w.primary.source.starts_with("invitems.c:"), "{} primary", w.name);
            let s = w.secondary.as_ref().expect("a gun has two functions");
            assert!(s.source.starts_with("invitems.c:"), "{} secondary", w.name);
        }
        assert_eq!(
            PD_WEAPONS.iter().filter(|w| w.primary.source.is_empty()).count(),
            1,
            "only MPWEAPON_SHIELD should need PdFunc::INERT"
        );
        for e in PD_EXPLOSIONS.iter() {
            assert!(e.source.starts_with("explosions.c:"), "{} has no source", e.name);
        }
    }
}
