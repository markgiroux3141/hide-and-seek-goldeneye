//! Enemy weapon definitions — the hunter side of the arsenal, ported from
//! `3DS FPS/src/data/EnemyWeaponConfig.ts` (the read-only oracle). An enemy holds
//! any of the player's [`WeaponStats`] guns, but attaches + animates them very
//! differently from the first-person viewmodel: the gun GLB is parented to a hand
//! **bone** (`Bone_9` right / `Bone_8` left) with a bone-local offset in GoldenEye
//! units (applied before the character's `CHAR_SCALE`), and the fire *animation* is
//! chosen by weapon **class** (pistol / rifle / dual-wield).
//!
//! The source defines exact bone offsets for only four guns (pp7, kf7, ar33,
//! rcp90). [`enemy_def_for`] reuses those verbatim and gives every other arsenal
//! weapon a per-class default offset (pp7's for pistols, kf7's for rifles), so any
//! of the 19 weapons can be equipped — and dual-wielded, since each class also
//! carries a mirrored left-hand offset.
//!
//! **Dual-wield is a runtime flag, not a weapon type** (JS `weaponOptions.dual`):
//! the same gun is attached to both hands, the left copy using
//! `left_offset`/`left_rot`, and both muzzles flash per shot. See [`crate::world`].

use glam::Vec3;

use super::config::WeaponStats;

/// How the enemy holds + fires a weapon. `Pistol` = one-handed (pistol fire anim);
/// `Rifle` = two-handed (rifle fire anim). Dual-wield is orthogonal (a `bool` flag
/// carried alongside the def) and overrides the fire anim to the dual clip.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnemyWeaponClass {
    Pistol,
    Rifle,
}

/// A weapon as an enemy wields it: the shared gun/muzzle/sound assets (identical to
/// the player [`WeaponStats`] paths), the AI fire stats, and the two bone-local
/// attach transforms (right hand always; left hand only when dual-wielding).
#[derive(Clone, Copy, Debug)]
pub struct EnemyWeaponDef {
    pub name: &'static str,
    pub class: EnemyWeaponClass,
    /// Gun GLB, relative under `native/assets/weapons/` (e.g. `"kf7/gun.glb"`).
    pub gun_path: &'static str,
    /// Muzzle-flash GLB (same root); `""` when the weapon has none (e.g. sniper).
    pub muzzle_path: &'static str,
    /// Fire SFX, relative under `native/assets/audio/`.
    pub fire_sound: &'static str,
    /// Damage per hit (JS `EnemyManager` uniform override — all hunters deal this).
    pub damage: f32,
    /// Base hit chance 0–1 (scaled by distance at the shot; JS `accuracy`).
    pub accuracy: f32,
    /// Effective range in metres (the hit roll goes to 0 beyond it).
    pub range: f32,
    /// Shots per second while inside the fire-animation window.
    pub fire_rate: f32,
    /// Right-hand (`Bone_9`) bone-local offset + XYZ-euler rotation, GE units.
    pub right_offset: Vec3,
    pub right_rot: Vec3,
    /// Left-hand (`Bone_8`) offset + rotation for the dual-wield copy, GE units.
    pub left_offset: Vec3,
    pub left_rot: Vec3,
}

/// Uniform enemy damage per hit (JS `EnemyManager.ts:56` `damage: 8`). Every hunter
/// deals this regardless of weapon; the weapon varies fire-rate / accuracy / range,
/// so DPS scales with the gun.
pub const ENEMY_DAMAGE: f32 = 8.0;

/// The right-hand attach bone (JS `Bone_9`), and the left-hand bone for the dual
/// copy (JS `Bone_8`).
pub const RIGHT_HAND_BONE: &str = "Bone_9";
pub const LEFT_HAND_BONE: &str = "Bone_8";

// ─── Bespoke source offsets (EnemyWeaponConfig.ts) ──────────────────────────────
// Pistol class defaults come from pp7; rifle class defaults from kf7; the dual
// (left-hand) rifle offset comes from rcp90 — the source's canonical dual weapon.
const PISTOL_R_OFF: Vec3 = Vec3::new(-150.0, 30.0, 115.0);
const PISTOL_R_ROT: Vec3 = Vec3::new(-0.39, -1.49, -1.84);
const PISTOL_L_OFF: Vec3 = Vec3::new(175.0, -30.0, 115.0);
const PISTOL_L_ROT: Vec3 = Vec3::new(3.11, 1.66, -1.49);

const RIFLE_R_OFF: Vec3 = Vec3::new(-90.0, 0.0, 145.0);
const RIFLE_R_ROT: Vec3 = Vec3::new(0.0, -1.49, -1.69);
// rcp90's left-hand offset — a plausible mirrored grip for any two-handed gun.
const RIFLE_L_OFF: Vec3 = Vec3::new(-145.0, 0.0, 0.0);
const RIFLE_L_ROT: Vec3 = Vec3::new(0.26, 1.56, 1.26);

/// Player-weapon names that an enemy holds one-handed (the pistol class). Every
/// other arsenal weapon is held two-handed (rifle class). Matches the JS split
/// (pistols one-handed, SMGs/rifles/shotguns/special two-handed).
const PISTOL_NAMES: &[&str] = &[
    "PP7",
    "DD44 Dostovei",
    "Cougar Magnum",
    "Golden Gun",
    "Gold PP7",
    "Silver PP7",
    "PP7 (Silenced)",
];

/// Build the enemy-side definition for any player [`WeaponStats`]. The four
/// source-defined guns (pp7/kf7/ar33/rcp90) get their exact bone offsets + AI
/// stats; every other weapon gets its class defaults so the full arsenal is
/// equippable. Asset paths + fire sound come straight off the player weapon (the
/// enemy and player share the same GLBs).
pub fn enemy_def_for(w: &WeaponStats) -> EnemyWeaponDef {
    let class = if PISTOL_NAMES.contains(&w.name) {
        EnemyWeaponClass::Pistol
    } else {
        EnemyWeaponClass::Rifle
    };

    // Class-default AI stats + offsets (pp7 for pistols, kf7 for rifles).
    let (accuracy, range, fire_rate, r_off, r_rot, l_off, l_rot) = match class {
        EnemyWeaponClass::Pistol => (
            0.85, 8.0, 2.0, PISTOL_R_OFF, PISTOL_R_ROT, PISTOL_L_OFF, PISTOL_L_ROT,
        ),
        EnemyWeaponClass::Rifle => (
            0.75, 12.0, 8.0, RIFLE_R_OFF, RIFLE_R_ROT, RIFLE_L_OFF, RIFLE_L_ROT,
        ),
    };

    // Bespoke overrides for the four source-defined guns (exact EnemyWeaponConfig.ts
    // values). `None` → keep the class defaults above.
    let bespoke: Option<(f32, f32, f32, Vec3, Vec3, Vec3, Vec3)> = match w.name {
        "PP7" => Some((
            0.85, 8.0, 2.0,
            Vec3::new(-150.0, 30.0, 115.0), Vec3::new(-0.39, -1.49, -1.84),
            Vec3::new(175.0, -30.0, 115.0), Vec3::new(3.11, 1.66, -1.49),
        )),
        "KF7 Soviet" => Some((
            0.75, 12.0, 8.0,
            Vec3::new(-90.0, 0.0, 145.0), Vec3::new(0.0, -1.49, -1.69),
            RIFLE_L_OFF, RIFLE_L_ROT, // no source dual data → mirrored default
        )),
        "AR33" => Some((
            0.8, 10.0, 6.0,
            Vec3::new(-90.0, 0.0, 145.0), Vec3::new(0.0, -1.49, -1.69),
            RIFLE_L_OFF, RIFLE_L_ROT,
        )),
        "RC-P90" => Some((
            0.7, 8.0, 12.0,
            Vec3::new(145.0, 0.0, 0.0), Vec3::new(0.0, -1.59, -1.59),
            Vec3::new(-145.0, 0.0, 0.0), Vec3::new(0.26, 1.56, 1.26),
        )),
        _ => None,
    };
    let (accuracy, range, fire_rate, r_off, r_rot, l_off, l_rot) =
        bespoke.unwrap_or((accuracy, range, fire_rate, r_off, r_rot, l_off, l_rot));

    EnemyWeaponDef {
        name: w.name,
        class,
        gun_path: w.gun_path,
        muzzle_path: w.muzzle_path,
        fire_sound: w.fire_sound,
        damage: ENEMY_DAMAGE,
        accuracy,
        range,
        fire_rate,
        right_offset: r_off,
        right_rot: r_rot,
        left_offset: l_off,
        left_rot: l_rot,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::config;

    #[test]
    fn pistols_are_one_handed_rest_two_handed() {
        assert_eq!(enemy_def_for(&config::PP7).class, EnemyWeaponClass::Pistol);
        assert_eq!(enemy_def_for(&config::DD44).class, EnemyWeaponClass::Pistol);
        assert_eq!(enemy_def_for(&config::KLOBB).class, EnemyWeaponClass::Rifle);
        assert_eq!(enemy_def_for(&config::KF7).class, EnemyWeaponClass::Rifle);
        assert_eq!(enemy_def_for(&config::SHOTGUN).class, EnemyWeaponClass::Rifle);
        assert_eq!(enemy_def_for(&config::LASER).class, EnemyWeaponClass::Rifle);
    }

    #[test]
    fn bespoke_offsets_match_source() {
        let kf7 = enemy_def_for(&config::KF7);
        assert_eq!(kf7.right_offset, Vec3::new(-90.0, 0.0, 145.0));
        assert_eq!(kf7.right_rot, Vec3::new(0.0, -1.49, -1.69));
        let rcp = enemy_def_for(&config::RCP90);
        assert_eq!(rcp.right_offset, Vec3::new(145.0, 0.0, 0.0));
        assert_eq!(rcp.left_offset, Vec3::new(-145.0, 0.0, 0.0));
    }

    #[test]
    fn every_weapon_has_a_nonzero_dual_left_offset() {
        // Any weapon must be dual-wieldable → a real (mirrored) left-hand grip.
        for w in config::WEAPONS {
            let d = enemy_def_for(w);
            assert!(
                d.left_offset.length_squared() > 1.0,
                "{} has a degenerate left-hand offset",
                d.name
            );
        }
    }

    #[test]
    fn assets_come_from_the_player_weapon() {
        let d = enemy_def_for(&config::KF7);
        assert_eq!(d.gun_path, config::KF7.gun_path);
        assert_eq!(d.fire_sound, config::KF7.fire_sound);
    }
}
