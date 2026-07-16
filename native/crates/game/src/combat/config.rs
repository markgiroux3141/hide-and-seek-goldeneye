//! Weapon stats — portable DATA transliterated from `src/weapons/WeaponConfig.ts`
//! (the read-only oracle). The 3DS FPS values are already in the ÷1000 metric
//! space (GoldenEye units → metres), so offsets/scale mirror the source directly;
//! expect to fine-tune the viewmodel transform live.
//!
//! Only the curated subset is defined here (P0 chose PP7 first). More weapons
//! come with weapon-switching (P6); the struct is the full `WeaponStats` shape so
//! adding one is just another const.

use glam::Vec3;

/// Static per-weapon configuration (JS `WeaponStats`). `sounds` are omitted until
/// an audio subsystem lands (`kira`/`rodio` are planned, not yet present).
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
    /// Relative asset path under `native/assets/weapons/` (gun GLB).
    pub gun_path: &'static str,
    /// Relative asset path (muzzle-flash GLB), empty when the weapon has none.
    pub muzzle_path: &'static str,
    /// Uniform scale applied to the gun mesh (GoldenEye units → view space).
    pub model_scale: f32,
    /// View-space placement of the gun (x right, y up, −z forward).
    pub model_offset: Vec3,
    /// Extra offset of the gun mesh within its pivot group (JS `pivotOffset`).
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
    /// click). Derived from the weapon: pistols/shotguns are semi, SMGs/rifles are
    /// auto. The JS reads mouse-down every frame and gates on `fireCooldown`, which
    /// is auto behaviour; semi-auto weapons additionally require an edge (a fresh
    /// click) — the native port makes that explicit here.
    pub automatic: bool,
}

/// PP7 — the semi-auto pistol (JS `PISTOL`). P0's first weapon: simplest fire
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
    gun_path: "pp7/gun.glb",
    muzzle_path: "pp7/muzzle.glb",
    model_scale: 0.0007,
    model_offset: Vec3::new(0.1, -0.08, -0.14),
    pivot_offset: Vec3::new(0.0, 0.0, -0.06),
    muzzle_offset: Vec3::new(0.05, 0.05, -0.3),
    model_rotation: Vec3::new(0.0, std::f32::consts::PI, 0.0),
    recoil_z: 0.03,
    recoil_rot: 0.26,
    automatic: false,
};

// NB: the JS `zoomFOV` (ADS/zoom) is deliberately not ported — the native camera
// has a fixed 60° FOV — so there's no zoom field here.
