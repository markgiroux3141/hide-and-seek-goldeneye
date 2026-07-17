//! Weapon stats — portable DATA transliterated from `src/weapons/WeaponConfig.ts`
//! (the read-only oracle). The 3DS FPS values are already in the ÷1000 metric
//! space (GoldenEye units → metres), so offsets/scale mirror the source directly.
//!
//! **Viewmodel placement comes from the tuned `public/config/weapon-config.json`**,
//! NOT the `WeaponConfig.ts` DEFAULT_* fallbacks. The JS loads that JSON at startup
//! (`loadWeaponOverrides`) and overrides `modelOffset` / `pivotOffset` /
//! `modelRotation` / `modelScale` per weapon — it's what the in-game weapon editor
//! saved, i.e. the positions the guns were actually hand-placed at. Native has no
//! weapon editor, so those final values are baked in as consts here. (`zoomFOV` from
//! that file is still not ported — the native camera is fixed 60° FOV.) The rest of
//! each weapon's stats (fire rate, mag, damage, range, recoil) come from the TS.
//!
//! The full GoldenEye arsenal (the JS `ALL_WEAPONS` array) is collected into
//! [`WEAPONS`], the inventory the player cycles with `Q` (keyboard) / `A` (N64 pad).
//! Asset paths are the JS ones with the `/models/weapons/` and leading-`/sounds/`
//! prefixes stripped — native resolves them under `native/assets/weapons/` and
//! `native/assets/audio/`.

use glam::Vec3;

/// Static per-weapon configuration (JS `WeaponStats`).
#[derive(Clone, Copy, Debug)]
pub struct WeaponStats {
    pub name: &'static str,
    /// Seconds between shots (fire-rate gate).
    pub fire_cooldown: f32,
    pub magazine_size: u32,
    pub reload_time: f32,
    pub damage: f32,
    /// Hitscan range in metres.
    pub range: f32,
    /// Audio asset names (relative to `native/assets/audio/`) for the fire,
    /// reload, and empty-click sounds (JS `sounds:{fire,reload,empty}`). Reload +
    /// empty are shared across weapons; fire is per-weapon. The `Weapon` queues
    /// these as sound cues the game layer plays through `engine::audio` — the
    /// volumes are fixed (see `combat::mod`'s `*_VOL` consts, mirroring JS).
    pub fire_sound: &'static str,
    pub reload_sound: &'static str,
    pub empty_sound: &'static str,
    /// Relative asset path under `native/assets/weapons/` (gun GLB).
    pub gun_path: &'static str,
    /// Relative asset path (muzzle-flash GLB), empty when the weapon has none.
    pub muzzle_path: &'static str,
    /// Uniform scale applied to the gun mesh (GoldenEye units → view space).
    pub model_scale: f32,
    /// View-space placement of the gun (x right, y up, −z forward). From the tuned
    /// `weapon-config.json` (see module docs).
    pub model_offset: Vec3,
    /// Extra offset of the gun mesh within its pivot group (JS `pivotOffset`). The
    /// `z` here is the main "stick-out" control — at rest the gun sits at
    /// `model_offset.z + pivot_offset.z` down the −Z (forward) axis.
    pub pivot_offset: Vec3,
    /// Muzzle tip offset (for the flash), view space.
    pub muzzle_offset: Vec3,
    /// Euler rotation of the gun mesh in radians (JS `modelRotation`, XYZ order).
    pub model_rotation: Vec3,
    /// Kick-back distance on fire (JS `recoilZ`).
    pub recoil_z: f32,
    /// Pitch-up rotation on fire in radians (JS `recoilRot`).
    pub recoil_rot: f32,
    /// True = automatic (fires while held); false = semi-auto (one shot per
    /// click). Derived from the weapon: pistols/shotguns/sniper are semi, SMGs/
    /// rifles/laser/auto-shotgun are auto. The JS reads mouse-down every frame and
    /// gates on `fireCooldown`, which is auto behaviour; semi-auto weapons
    /// additionally require an edge (a fresh click) — the native port makes that
    /// explicit here.
    pub automatic: bool,
}

// ─── Shared viewmodel placement ───────────────────────────────────────────────
// Every weapon's muzzle_offset + model_scale matched these in `weapon-config.json`,
// so they stay shared; model_offset / pivot_offset / model_rotation are per-weapon.
const DEFAULT_MUZZLE: Vec3 = Vec3::new(0.05, 0.05, -0.3);
const DEFAULT_SCALE: f32 = 0.0007;
/// Radians per degree — the tuned rotations are whole-degree tweaks about the
/// straight-back yaw (`PI`); expressing them as `PI ± N·DEG` keeps intent legible.
const DEG: f32 = std::f32::consts::PI / 180.0;
const PI: f32 = std::f32::consts::PI;

const RELOAD_SND: &str = "sounds/weapons/reload.wav";
const EMPTY_SND: &str = "sounds/weapons/empty.wav";

// ─── Pistols (semi-auto) ──────────────────────────────────────────────────────

/// PP7 — the semi-auto pistol (JS `PISTOL`). The starting weapon: simplest fire
/// path (edge-triggered), punchy recoil (`recoil_rot` 0.26), small mag for fast
/// reload verification.
pub const PP7: WeaponStats = WeaponStats {
    name: "PP7",
    fire_cooldown: 0.4,
    magazine_size: 7,
    // ~half the JS 1.5 s (user call 2026-07-16) — snappier reload; also shortens
    // the viewmodel dip, which spans `reload_time`.
    reload_time: 0.75,
    damage: 25.0,
    range: 100.0,
    fire_sound: "sounds/weapons/pp7-fire.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "pp7/gun.glb",
    muzzle_path: "pp7/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.1, -0.08, -0.04),
    pivot_offset: Vec3::new(0.0, 0.0, -0.16),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(2.0 * DEG, PI, 0.0),
    recoil_z: 0.03,
    recoil_rot: 0.26,
    automatic: false,
};

pub const DD44: WeaponStats = WeaponStats {
    name: "DD44 Dostovei",
    fire_cooldown: 0.4,
    magazine_size: 8,
    reload_time: 1.5,
    damage: 20.0,
    range: 80.0,
    fire_sound: "sounds/weapons/dd44-fire.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "dd44/gun.glb",
    muzzle_path: "dd44/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.11, -0.09, -0.09),
    pivot_offset: Vec3::new(0.0, 0.0, -0.26),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(1.0 * DEG, PI - 2.0 * DEG, 0.0),
    recoil_z: 0.03,
    recoil_rot: 0.26,
    automatic: false,
};

pub const MAGNUM: WeaponStats = WeaponStats {
    name: "Cougar Magnum",
    fire_cooldown: 0.6,
    magazine_size: 6,
    reload_time: 1.5,
    damage: 50.0,
    range: 100.0,
    fire_sound: "sounds/weapons/magnum-fire.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "magnum/gun.glb",
    muzzle_path: "magnum/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.11, -0.11, -0.05),
    pivot_offset: Vec3::new(0.0, 0.0, -0.26),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(1.0 * DEG, PI, 0.0),
    recoil_z: 0.03,
    recoil_rot: 0.26,
    automatic: false,
};

pub const GOLDEN_GUN: WeaponStats = WeaponStats {
    name: "Golden Gun",
    fire_cooldown: 1.0,
    magazine_size: 1,
    reload_time: 1.0,
    damage: 999.0,
    range: 200.0,
    fire_sound: "sounds/weapons/pp7-fire.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "golden-gun/gun.glb",
    muzzle_path: "golden-gun/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.1, -0.1, -0.04),
    pivot_offset: Vec3::new(0.0, 0.0, -0.26),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(3.0 * DEG, PI, 0.0),
    recoil_z: 0.03,
    recoil_rot: 0.26,
    automatic: false,
};

pub const GOLD_PP7: WeaponStats = WeaponStats {
    name: "Gold PP7",
    fire_cooldown: 0.4,
    magazine_size: 7,
    reload_time: 1.5,
    damage: 25.0,
    range: 100.0,
    fire_sound: "sounds/weapons/pp7-fire.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "gold-pp7/gun.glb",
    muzzle_path: "gold-pp7/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.1, -0.1, -0.12),
    pivot_offset: Vec3::new(0.0, 0.0, -0.11),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(2.0 * DEG, PI, 0.0),
    recoil_z: 0.03,
    recoil_rot: 0.26,
    automatic: false,
};

pub const SILVER_PP7: WeaponStats = WeaponStats {
    name: "Silver PP7",
    fire_cooldown: 0.4,
    magazine_size: 7,
    reload_time: 1.5,
    damage: 25.0,
    range: 100.0,
    fire_sound: "sounds/weapons/pp7-fire.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "silver-pp7/gun.glb",
    muzzle_path: "silver-pp7/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.1, -0.1, -0.12),
    pivot_offset: Vec3::new(0.0, 0.0, -0.1),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(2.0 * DEG, PI + 1.0 * DEG, 0.0),
    recoil_z: 0.03,
    recoil_rot: 0.26,
    automatic: false,
};

pub const PP7_SILENCER: WeaponStats = WeaponStats {
    name: "PP7 (Silenced)",
    fire_cooldown: 0.4,
    magazine_size: 7,
    reload_time: 1.5,
    damage: 25.0,
    range: 100.0,
    fire_sound: "sounds/weapons/silencer-pistol.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "pp7-silencer/gun.glb",
    muzzle_path: "pp7-silencer/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.1, -0.1, -0.07),
    pivot_offset: Vec3::new(0.0, 0.0, -0.16),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(0.0, PI, 0.0),
    recoil_z: 0.03,
    recoil_rot: 0.26,
    automatic: false,
};

// ─── SMGs (automatic) ─────────────────────────────────────────────────────────

pub const KLOBB: WeaponStats = WeaponStats {
    name: "Klobb",
    fire_cooldown: 0.1,
    magazine_size: 20,
    reload_time: 2.0,
    damage: 5.0,
    range: 50.0,
    fire_sound: "sounds/weapons/klobb-fire.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "klobb/gun.glb",
    muzzle_path: "klobb/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.1, -0.1, -0.06),
    pivot_offset: Vec3::new(0.0, 0.0, -0.12),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(0.0, PI, 0.0),
    recoil_z: 0.02,
    recoil_rot: 0.03,
    automatic: true,
};

pub const DK5: WeaponStats = WeaponStats {
    name: "D5K Deutsche",
    fire_cooldown: 0.08,
    magazine_size: 30,
    reload_time: 2.0,
    damage: 8.0,
    range: 60.0,
    fire_sound: "sounds/weapons/dk5-fire.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "dk5/gun.glb",
    muzzle_path: "dk5/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.12, -0.14, -0.1),
    pivot_offset: Vec3::new(0.0, 0.0, -0.06),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(0.0, PI + 1.0 * DEG, 0.0),
    recoil_z: 0.02,
    recoil_rot: 0.03,
    automatic: true,
};

pub const DK5_SILENCER: WeaponStats = WeaponStats {
    name: "D5K (Silenced)",
    fire_cooldown: 0.08,
    magazine_size: 30,
    reload_time: 2.0,
    damage: 8.0,
    range: 60.0,
    fire_sound: "sounds/weapons/silencer-pistol.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "dk5-silencer/gun.glb",
    muzzle_path: "dk5-silencer/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.11, -0.14, -0.1),
    pivot_offset: Vec3::new(0.0, 0.0, -0.06),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(0.0, PI, 0.0),
    recoil_z: 0.02,
    recoil_rot: 0.03,
    automatic: true,
};

pub const PHANTOM: WeaponStats = WeaponStats {
    name: "Phantom",
    fire_cooldown: 0.06,
    magazine_size: 50,
    reload_time: 2.0,
    damage: 8.0,
    range: 60.0,
    fire_sound: "sounds/weapons/k47-fire.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "phantom/gun.glb",
    muzzle_path: "phantom/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.11, -0.13, -0.09),
    pivot_offset: Vec3::new(0.0, 0.0, -0.36),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(0.0, PI, 0.0),
    recoil_z: 0.02,
    recoil_rot: 0.03,
    automatic: true,
};

pub const ZMG: WeaponStats = WeaponStats {
    name: "ZMG 9mm",
    fire_cooldown: 0.06,
    magazine_size: 32,
    reload_time: 2.0,
    damage: 8.0,
    range: 60.0,
    fire_sound: "sounds/weapons/k47-fire.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "zmgobj/gun.glb",
    muzzle_path: "zmgobj/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.1, -0.1, -0.08),
    pivot_offset: Vec3::new(0.0, 0.0, -0.11),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(0.0, PI, 0.0),
    recoil_z: 0.02,
    recoil_rot: 0.03,
    automatic: true,
};

// ─── Rifles (automatic) ───────────────────────────────────────────────────────

pub const RCP90: WeaponStats = WeaponStats {
    name: "RC-P90",
    fire_cooldown: 0.07,
    magazine_size: 80,
    reload_time: 2.0,
    damage: 10.0,
    range: 80.0,
    fire_sound: "sounds/weapons/rcp90-fire.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "rcp-90/gun.glb",
    muzzle_path: "rcp-90/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.09, -0.12, 0.0),
    pivot_offset: Vec3::new(0.0, 0.0, 0.07),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(0.0, PI, 0.0),
    recoil_z: 0.02,
    recoil_rot: 0.03,
    automatic: true,
};

pub const AR33: WeaponStats = WeaponStats {
    name: "AR33",
    fire_cooldown: 0.1,
    magazine_size: 30,
    reload_time: 2.0,
    damage: 15.0,
    range: 90.0,
    fire_sound: "sounds/weapons/ar33-fire.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "ar33/gun.glb",
    muzzle_path: "ar33/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.11, -0.13, -0.03),
    pivot_offset: Vec3::new(0.0, 0.0, -0.13),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(-1.0 * DEG, PI - 2.0 * DEG, 0.0),
    recoil_z: 0.02,
    recoil_rot: 0.03,
    automatic: true,
};

/// KF7 Soviet — also the hunter's rifle (see `world`'s `ENEMY_*` overrides, which
/// re-tune damage/range for the AI). Player copy uses the JS `KF7` stats; the
/// viewmodel placement here is the player-held tuning (the enemy attaches the same
/// GLB with its own bone-local offset).
pub const KF7: WeaponStats = WeaponStats {
    name: "KF7 Soviet",
    fire_cooldown: 0.12,
    magazine_size: 30,
    reload_time: 2.0,
    damage: 15.0,
    range: 85.0,
    fire_sound: "sounds/weapons/k47-fire.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "kf7/gun.glb",
    muzzle_path: "kf7/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.12, -0.14, -0.05),
    pivot_offset: Vec3::new(0.0, 0.0, -0.1),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(-4.0 * DEG, PI - 7.0 * DEG, 0.0),
    recoil_z: 0.02,
    recoil_rot: 0.03,
    automatic: true,
};

// ─── Shotguns ─────────────────────────────────────────────────────────────────

/// Pump shotgun — semi (one shot per pull, JS `SHOTGUN`).
pub const SHOTGUN: WeaponStats = WeaponStats {
    name: "Shotgun",
    fire_cooldown: 0.8,
    magazine_size: 5,
    reload_time: 3.0,
    damage: 50.0,
    range: 25.0,
    fire_sound: "sounds/weapons/shotgun-fire.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "shotgun/gun.glb",
    muzzle_path: "shotgun/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.11, -0.1, -0.1),
    pivot_offset: Vec3::new(0.0, 0.0, -0.06),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(0.0, PI, 0.0),
    recoil_z: 0.04,
    recoil_rot: 0.06,
    automatic: false,
};

/// Automatic shotgun — full-auto (JS `AUTO_SHOTGUN`).
pub const AUTO_SHOTGUN: WeaponStats = WeaponStats {
    name: "Auto Shotgun",
    fire_cooldown: 0.25,
    magazine_size: 5,
    reload_time: 2.5,
    damage: 40.0,
    range: 30.0,
    fire_sound: "sounds/weapons/auto-shotgun-fire.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "auto-shotgun/gun.glb",
    muzzle_path: "auto-shotgun/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.11, -0.15, -0.1),
    pivot_offset: Vec3::new(0.0, 0.0, -0.06),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(0.0, PI, 0.0),
    recoil_z: 0.04,
    recoil_rot: 0.06,
    automatic: true,
};

// ─── Special ──────────────────────────────────────────────────────────────────

/// Sniper Rifle — semi, no muzzle flash (JS `muzzleFlashPath: ''`). JS also gave
/// it a 25° `zoomFOV`; the native camera is fixed 60° so zoom is not ported.
pub const SNIPER: WeaponStats = WeaponStats {
    name: "Sniper Rifle",
    fire_cooldown: 1.2,
    magazine_size: 8,
    reload_time: 2.5,
    damage: 100.0,
    range: 200.0,
    fire_sound: "sounds/weapons/silencer-pistol.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "sniper/gun.glb",
    muzzle_path: "",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.12, -0.14, -0.17),
    pivot_offset: Vec3::new(0.0, 0.0, -0.06),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(-1.0 * DEG, PI, 0.0),
    recoil_z: 0.02,
    recoil_rot: 0.03,
    automatic: false,
};

/// Moonraker Laser — full-auto, huge mag (JS `LASER`).
pub const LASER: WeaponStats = WeaponStats {
    name: "Moonraker Laser",
    fire_cooldown: 0.05,
    magazine_size: 800,
    reload_time: 3.0,
    damage: 5.0,
    range: 150.0,
    fire_sound: "sounds/weapons/laser-fire.wav",
    reload_sound: RELOAD_SND,
    empty_sound: EMPTY_SND,
    gun_path: "laser/gun.glb",
    muzzle_path: "laser/muzzle.glb",
    model_scale: DEFAULT_SCALE,
    model_offset: Vec3::new(0.07, -0.09, -0.1),
    pivot_offset: Vec3::new(0.0, 0.0, -0.26),
    muzzle_offset: DEFAULT_MUZZLE,
    model_rotation: Vec3::new(2.0 * DEG, PI + 4.0 * DEG, 0.0),
    recoil_z: 0.02,
    recoil_rot: 0.03,
    automatic: true,
};

/// The player's cycle-order inventory (JS `ALL_WEAPONS`): pistols → PP7 variants →
/// SMGs → rifles → shotguns → special. Index 0 (PP7) is the weapon spawned first.
/// `Q` (keyboard) / `A` (N64 pad) steps through this list.
pub const WEAPONS: &[WeaponStats] = &[
    PP7, DD44, MAGNUM, GOLDEN_GUN, // pistols
    GOLD_PP7, SILVER_PP7, PP7_SILENCER, // pp7 variants
    KLOBB, DK5, DK5_SILENCER, PHANTOM, ZMG, // smgs
    RCP90, AR33, KF7, // rifles
    SHOTGUN, AUTO_SHOTGUN, // shotguns
    SNIPER, LASER, // special
];

// NB: the JS `zoomFOV` (ADS/zoom) is deliberately not ported — the native camera
// has a fixed 60° FOV — so there's no zoom field here.
